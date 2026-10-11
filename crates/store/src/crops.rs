//! Crop plans, their successions, and what's due.

use std::collections::HashMap;

use homestead_domain::crops::{CropPlan, Due, NewCropPlan, PlantingStep, YieldComparison};
use homestead_domain::{Event, HarvestUnit, Planting, SowMethod};
use rust_decimal::Decimal;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{PlantingRow, Result, Store, StoreError, Tx, parse, planting_cols};

macro_rules! plan_cols {
    () => {
        "id, crop, variety, family, notes, method, days_to_transplant, days_to_harvest, harvest_window_days,
         seed_lot_id, expected_yield, expected_yield_unit from homestead.crop_plans"
    };
}

#[derive(sqlx::FromRow)]
struct PlanRow {
    id: Uuid,
    crop: String,
    variety: String,
    family: String,
    notes: String,
    method: String,
    days_to_transplant: Option<i32>,
    days_to_harvest: Option<i32>,
    harvest_window_days: Option<i32>,
    seed_lot_id: Option<Uuid>,
    expected_yield: Option<Decimal>,
    expected_yield_unit: Option<String>,
}

impl TryFrom<PlanRow> for CropPlan {
    type Error = StoreError;
    fn try_from(r: PlanRow) -> Result<CropPlan> {
        let days = |d: Option<i32>| d.map(|d| u16::try_from(d).map_err(|_| StoreError::Corrupt("plan days".into())));
        Ok(CropPlan {
            id: r.id,
            crop: r.crop,
            variety: r.variety,
            family: r.family,
            notes: r.notes,
            method: parse(&r.method)?,
            days_to_transplant: days(r.days_to_transplant).transpose()?,
            days_to_harvest: days(r.days_to_harvest).transpose()?,
            harvest_window_days: days(r.harvest_window_days).transpose()?,
            seed_lot_id: r.seed_lot_id,
            expected_yield: r.expected_yield,
            expected_yield_unit: r.expected_yield_unit.as_deref().map(parse).transpose()?,
        })
    }
}

/// A plan in the list, with how its successions are getting on.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct PlanSummary {
    pub id: Uuid,
    pub crop: String,
    pub variety: String,
    pub first_sow_on: Option<Date>,
    pub successions: i64,
    pub planned: i64,
    pub in_ground: i64,
    pub over: i64,
    pub created_at: OffsetDateTime,
}

/// One succession of a plan, and how its yield compares with the plan's.
pub struct PlanSuccession {
    pub planting: Planting,
    pub place: String,
    pub yields: YieldComparison,
}

/// What the crops need now.
pub struct CropAgenda {
    pub today: Date,
    /// Plantings due to be sown (overdue first), then harvests coming due or overdue.
    pub plantings: Vec<(Planting, String, Due)>,
    /// Open transplant tasks due within a week: task id, due date, planting, place.
    pub transplants: Vec<(Uuid, Date, Planting, String)>,
}

impl Store {
    /// A plan and, for each succession, a planned planting with its sowing
    /// (and transplanting) as ordinary tasks. Nothing is announced: the
    /// planting is announced when it goes in the ground, not when it's planned.
    pub async fn create_crop_plan(&self, input: NewCropPlan) -> Result<CropPlan> {
        let plan = input.validated()?;
        let mut tx = self.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "insert into homestead.crop_plans (crop, variety, family, notes, method, days_to_transplant,
               days_to_harvest, harvest_window_days, seed_lot_id, expected_yield, expected_yield_unit)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) returning id",
        )
        .bind(&plan.crop)
        .bind(&plan.variety)
        .bind(&plan.family)
        .bind(&plan.notes)
        .bind(plan.method.as_str())
        .bind(plan.days_to_transplant.map(i32::from))
        .bind(plan.days_to_harvest.map(i32::from))
        .bind(plan.harvest_window_days.map(i32::from))
        .bind(plan.seed_lot_id)
        .bind(plan.expected_yield)
        .bind(plan.expected_yield_unit.map(|u| u.as_str()))
        .fetch_one(&mut *tx)
        .await?;
        for s in &plan.successions {
            let place: String = sqlx::query_scalar("select name from homestead.locations where id = $1")
                .bind(s.location_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound("location"))?;
            let t = plan.timing(s.sow_on);
            let planting: Uuid = sqlx::query_scalar(
                "insert into homestead.plantings (location_id, crop, variety, sown_on, expected_harvest_on,
                   expected_harvest_until, transplant_on, status, crop_plan_id, family, method, seed_lot_id,
                   expected_yield, expected_yield_unit)
                 values ($1,$2,$3,$4,$5,$6,$7,'planned',$8,$9,$10,$11,$12,$13) returning id",
            )
            .bind(s.location_id)
            .bind(&plan.crop)
            .bind(&plan.variety)
            .bind(s.sow_on)
            .bind(t.harvest_from)
            .bind(t.harvest_until)
            .bind(t.transplant_on)
            .bind(id)
            .bind(&plan.family)
            .bind(plan.method.as_str())
            .bind(plan.seed_lot_id)
            .bind(plan.expected_yield)
            .bind(plan.expected_yield_unit.map(|u| u.as_str()))
            .fetch_one(&mut *tx)
            .await?;
            let sow = if plan.method == SowMethod::Transplant { "Start seed for" } else { "Sow" };
            step_task(
                &mut tx,
                planting,
                PlantingStep::Sow,
                &format!("{sow} {}", plan.crop),
                &place,
                s.location_id,
                s.sow_on,
            )
            .await?;
            if let Some(on) = t.transplant_on {
                let title = format!("Transplant {}", plan.crop);
                step_task(&mut tx, planting, PlantingStep::Transplant, &title, &place, s.location_id, on).await?;
            }
        }
        let row: PlanRow =
            sqlx::query_as(concat!("select ", plan_cols!(), " where id = $1")).bind(id).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        row.try_into()
    }

    pub async fn get_crop_plan(&self, id: Uuid) -> Result<(CropPlan, Vec<PlanSuccession>)> {
        let row: PlanRow = sqlx::query_as(concat!("select ", plan_cols!(), " where id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound("crop plan"))?;
        let plan: CropPlan = row.try_into()?;
        let rows: Vec<PlantingRow> = sqlx::query_as(concat!(
            "select ",
            planting_cols!(),
            " where p.crop_plan_id = $1 order by p.sown_on, l.name"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let mut harvested = self.harvested_by_planting(&ids).await?;
        let successions = rows
            .into_iter()
            .map(|r| {
                let (planting, place) = r.with_place()?;
                let expected = planting.expected_yield.zip(planting.expected_yield_unit);
                let yields = YieldComparison::new(expected, &harvested.remove(&planting.id).unwrap_or_default());
                Ok(PlanSuccession { planting, place, yields })
            })
            .collect::<Result<_>>()?;
        Ok((plan, successions))
    }

    pub async fn list_crop_plans(&self) -> Result<Vec<PlanSummary>> {
        Ok(sqlx::query_as(
            "select c.id, c.crop, c.variety, min(p.sown_on) as first_sow_on, count(p.id) as successions,
                    count(p.id) filter (where p.status = 'planned') as planned,
                    count(p.id) filter (where p.status in ('growing', 'harvesting')) as in_ground,
                    count(p.id) filter (where p.status in ('finished', 'failed')) as over,
                    c.created_at
             from homestead.crop_plans c left join homestead.plantings p on p.crop_plan_id = c.id
             group by c.id order by (count(p.id) filter (where p.status in ('finished', 'failed')) = count(p.id)),
               min(p.sown_on) desc nulls last, c.crop",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Harvested quantities per planting, one entry per unit.
    pub(crate) async fn harvested_by_planting(
        &self,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<(Decimal, HarvestUnit)>>> {
        let rows: Vec<(Uuid, String, Decimal)> = sqlx::query_as(
            "select planting_id, unit, sum(quantity) from homestead.harvests
             where planting_id = any($1) group by planting_id, unit order by planting_id, unit",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        let mut out: HashMap<Uuid, Vec<(Decimal, HarvestUnit)>> = HashMap::new();
        for (id, unit, q) in rows {
            out.entry(id).or_default().push((q, parse(&unit)?));
        }
        Ok(out)
    }

    /// Sowings due this week or overdue, harvests due within two weeks or
    /// overdue, and transplanting due this week, as of the farm's today.
    pub async fn crop_agenda(&self) -> Result<CropAgenda> {
        let today = self.today().await?;
        let rows: Vec<PlantingRow> = sqlx::query_as(concat!(
            "select ",
            planting_cols!(),
            " where (p.status = 'planned' and p.sown_on <= $1 + 6)
                 or (p.status = 'growing' and p.expected_harvest_on <= $1 + 13)
             order by p.sown_on, l.name"
        ))
        .bind(today)
        .fetch_all(&self.pool)
        .await?;
        let mut plantings = Vec::new();
        for r in rows {
            let (p, place) = r.with_place()?;
            if let Some(due) = Due::of(&p, today) {
                plantings.push((p, place, due));
            }
        }
        plantings.sort_by_key(|(p, _, due)| (*due, p.expected_harvest_on.unwrap_or(p.sown_on)));
        #[derive(sqlx::FromRow)]
        struct TransplantRow {
            task_id: Uuid,
            due_on: Date,
            #[sqlx(flatten)]
            planting: PlantingRow,
        }
        let rows: Vec<TransplantRow> = sqlx::query_as(concat!(
            "select t.id as task_id, t.due_on, ",
            planting_cols!(),
            " join homestead.tasks t on t.planting_id = p.id and t.planting_step = 'transplant'
             where t.status = 'open' and t.due_on <= $1 + 6 order by t.due_on, l.name"
        ))
        .bind(today)
        .fetch_all(&self.pool)
        .await?;
        let transplants = rows
            .into_iter()
            .map(|r| {
                let (p, place) = r.planting.with_place()?;
                Ok((r.task_id, r.due_on, p, place))
            })
            .collect::<Result<_>>()?;
        Ok(CropAgenda { today, plantings, transplants })
    }
}

async fn step_task(
    tx: &mut Tx<'_>,
    planting: Uuid,
    step: PlantingStep,
    title: &str,
    place: &str,
    location_id: Uuid,
    due_on: Date,
) -> Result<()> {
    sqlx::query(
        "insert into homestead.tasks (title, details, location_id, due_on, priority, planting_id, planting_step)
         values ($1, $2, $3, $4, 'normal', $5, $6)",
    )
    .bind(title)
    .bind(format!("Planned in {place}"))
    .bind(location_id)
    .bind(due_on)
    .bind(planting)
    .bind(step.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records what a completed planting task did: a sowing puts a planned
/// planting in the ground today and moves its later dates by as many days
/// as it was early or late; a transplant records the day. Returns the
/// announcement when a planting went in the ground.
pub(crate) async fn planting_step_done(tx: &mut Tx<'_>, task_id: Uuid, today: Date) -> Result<Option<Event>> {
    let found: Option<(String, Uuid, String, Date)> = sqlx::query_as(
        "select t.planting_step, p.id, p.status, p.sown_on from homestead.tasks t
         join homestead.plantings p on p.id = t.planting_id where t.id = $1 for update of p",
    )
    .bind(task_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((step, planting, status, sown_on)) = found else { return Ok(None) };
    let step: PlantingStep = parse(&step)?;
    let planned = status == "planned";
    match step {
        PlantingStep::Sow if planned => {
            let shift =
                i32::try_from((today - sown_on).whole_days()).map_err(|_| StoreError::Corrupt("date".into()))?;
            sqlx::query(
                "update homestead.plantings set status = 'growing', sown_on = $2,
                   transplant_on = transplant_on + $3, expected_harvest_on = expected_harvest_on + $3,
                   expected_harvest_until = expected_harvest_until + $3
                 where id = $1",
            )
            .bind(planting)
            .bind(today)
            .bind(shift)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "update homestead.tasks set due_on = due_on + $2
                 where planting_id = $1 and planting_step = 'transplant' and status = 'open'",
            )
            .bind(planting)
            .bind(shift)
            .execute(&mut **tx)
            .await?;
        }
        PlantingStep::Sow => return Ok(None),
        PlantingStep::Transplant => {
            sqlx::query(
                "update homestead.plantings set transplant_on = greatest($2, sown_on),
                   status = case when status = 'planned' then 'growing' else status end
                 where id = $1",
            )
            .bind(planting)
            .bind(today)
            .execute(&mut **tx)
            .await?;
            if !planned {
                return Ok(None);
            }
        }
    }
    Ok(Some(crate::planting_created(tx, planting).await?))
}
