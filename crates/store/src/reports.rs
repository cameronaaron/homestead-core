//! The fiscal-year report, computed from the operational records each
//! time it's asked for. Only the sections asked for are queried.

use homestead_domain::reports::{
    Animals, Costs, Count, Events, FiscalYear, Gifts, Harvest, Incidents, Inventory, Measure, Money, Report,
    ReportSection, Stand, Volunteers,
};
use rust_decimal::Decimal;
use time::Date;

use crate::{Result, Store};

fn counts(rows: Vec<(String, i64)>) -> Vec<Count> {
    rows.into_iter().map(|(key, count)| Count { key, count }).collect()
}

fn measures(rows: Vec<(String, String, Decimal)>) -> Vec<Measure> {
    rows.into_iter().map(|(key, unit, quantity)| Measure { key, unit, quantity }).collect()
}

fn by_unit(rows: Vec<(String, Decimal)>) -> Vec<Measure> {
    rows.into_iter().map(|(unit, quantity)| Measure { key: String::new(), unit, quantity }).collect()
}

impl Store {
    /// `fy` from its first day through `through`, with only `sections`
    /// filled in. The caller picks the sections by capability
    /// (`ReportSection::visible`).
    pub async fn fiscal_report(&self, fy: FiscalYear, through: Date, sections: &[ReportSection]) -> Result<Report> {
        let from = fy.first_day();
        let want = |s| sections.contains(&s);
        Ok(Report {
            fiscal_year: fy,
            from,
            through,
            volunteers: if want(ReportSection::Volunteers) {
                Some(self.volunteers(from, through).await?)
            } else {
                None
            },
            events: if want(ReportSection::Events) { Some(self.events_held(from, through).await?) } else { None },
            animals: if want(ReportSection::Animals) {
                Some(self.animals_assisted(from, through).await?)
            } else {
                None
            },
            harvest: if want(ReportSection::Harvest) { Some(self.harvest_totals(from, through).await?) } else { None },
            stand: if want(ReportSection::Stand) { Some(self.stand_totals(from, through).await?) } else { None },
            gifts: if want(ReportSection::Gifts) { Some(self.gift_totals(from, through).await?) } else { None },
            inventory: if want(ReportSection::Inventory) {
                Some(self.inventory_usage(from, through).await?)
            } else {
                None
            },
            costs: if want(ReportSection::Costs) { Some(self.cost_totals(from, through).await?) } else { None },
            incidents: if want(ReportSection::Incidents) {
                Some(self.incident_totals(from, through).await?)
            } else {
                None
            },
        })
    }

    async fn volunteers(&self, from: Date, to: Date) -> Result<Volunteers> {
        let (hours, shifts, people): (Decimal, i64, i64) = sqlx::query_as(
            "select coalesce(sum(hours), 0), count(*), count(distinct volunteer_id)
             from homestead.volunteer_shifts where worked_on between $1 and $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;
        let groups: Vec<(String, Decimal)> = sqlx::query_as(
            "select working_group, sum(hours) from homestead.volunteer_shifts
             where worked_on between $1 and $2 group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let hours_by_group = groups.into_iter().map(|(key, amount)| Money { key, amount }).collect();
        Ok(Volunteers { hours, shifts, people, hours_by_group })
    }

    async fn events_held(&self, from: Date, to: Date) -> Result<Events> {
        let by_kind: Vec<(String, i64)> = sqlx::query_as(
            "select kind, count(*) from homestead.community_events where held_on between $1 and $2 group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let (attendance, without_attendance): (i64, i64) = sqlx::query_as(
            "select coalesce(sum(attendance), 0)::bigint, count(*) filter (where attendance is null)
             from homestead.community_events where held_on between $1 and $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;
        let by_kind = counts(by_kind);
        Ok(Events { held: by_kind.iter().map(|k| k.count).sum(), by_kind, attendance, without_attendance })
    }

    async fn animals_assisted(&self, from: Date, to: Date) -> Result<Animals> {
        // In care at some point in [from, to]: arrived in it, placed in it,
        // or arrived by its end and still in our care now (so in care on
        // its last day). Current status only ever widens "still in care",
        // never invents a past, so this is a lower bound.
        let assisted: Vec<(String, i64)> = sqlx::query_as(
            "select species, sum(head_count)::bigint from homestead.animals a
             where arrived_on <= $2
               and (arrived_on >= $1
                    or status in ('resident', 'in_care', 'fostered', 'adoptable')
                    or exists (select 1 from homestead.animal_placements p
                               where p.animal_id = a.id and p.started_on between $1 and $2))
             group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let intake: Vec<(String, i64)> = sqlx::query_as(
            "select species, sum(head_count)::bigint from homestead.animals
             where arrived_on between $1 and $2 group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let placements: Vec<(String, i64)> = sqlx::query_as(
            "select p.kind, sum(a.head_count)::bigint from homestead.animal_placements p
             join homestead.animals a on a.id = p.animal_id
             where p.started_on between $1 and $2 group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let (eggs_collected, eggs_damaged): (i64, i64) = sqlx::query_as(
            "select coalesce(sum(collected), 0)::bigint, coalesce(sum(damaged), 0)::bigint
             from homestead.egg_days where on_date between $1 and $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;
        Ok(Animals {
            assisted: counts(assisted),
            intake: counts(intake),
            placements: counts(placements),
            eggs_collected,
            eggs_damaged,
        })
    }

    async fn harvest_totals(&self, from: Date, to: Date) -> Result<Harvest> {
        let harvested: Vec<(String, String, Decimal)> = sqlx::query_as(
            "select p.crop, h.unit, sum(h.quantity) from homestead.harvests h
             join homestead.plantings p on p.id = h.planting_id
             where h.harvested_on between $1 and $2 group by 1, 2 order by 1, 2",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let dispositions: Vec<(String, String, Decimal)> = sqlx::query_as(
            "select d.kind, d.unit, sum(d.quantity) from homestead.harvest_dispositions d
             join homestead.harvests h on h.id = d.harvest_id
             where h.harvested_on between $1 and $2 group by 1, 2 order by 1, 2",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let donated: Vec<(String, Decimal)> = sqlx::query_as(
            "select unit, sum(quantity) from homestead.produce_donations
             where donated_on between $1 and $2 group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let losses: Vec<(String, Decimal)> = sqlx::query_as(
            "select unit, sum(quantity) from homestead.losses
             where lost_on between $1 and $2 and quantity is not null group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(Harvest {
            harvested: measures(harvested),
            dispositions: measures(dispositions),
            donated: by_unit(donated),
            losses: by_unit(losses),
        })
    }

    async fn stand_totals(&self, from: Date, to: Date) -> Result<Stand> {
        let (revenue, sales): (Decimal, i64) = sqlx::query_as(
            "select coalesce(sum(amount), 0), count(*) from homestead.stand_sales where sold_on between $1 and $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;
        let sold: Vec<(String, String, Decimal)> = sqlx::query_as(
            "select item, unit, sum(quantity) from homestead.stand_sales
             where sold_on between $1 and $2 group by 1, 2 order by 1, 2",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let (compost_dropoffs, compost_lb): (i64, Decimal) = sqlx::query_as(
            "select count(*), coalesce(sum(weight_lb), 0) from homestead.compost_dropoffs
             where received_on between $1 and $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;
        Ok(Stand { revenue, sales, sold: measures(sold), compost_dropoffs, compost_lb })
    }

    async fn gift_totals(&self, from: Date, to: Date) -> Result<Gifts> {
        let row: (Decimal, i64, Decimal, i64, i64, Decimal, i64) = sqlx::query_as(
            "select coalesce(sum(amount) filter (where kind = 'cash'), 0), count(*) filter (where kind = 'cash'),
                    coalesce(sum(amount) filter (where kind = 'sponsorship'), 0),
                    count(*) filter (where kind = 'sponsorship'),
                    count(*) filter (where kind = 'in_kind'), coalesce(sum(estimated_value), 0),
                    count(*) filter (where kind = 'in_kind' and estimated_value is null)
             from homestead.gifts where received_on between $1 and $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await?;
        Ok(Gifts {
            cash: Money { key: "cash".into(), amount: row.0 },
            cash_count: row.1,
            sponsorship: Money { key: "sponsorship".into(), amount: row.2 },
            sponsorship_count: row.3,
            in_kind_count: row.4,
            in_kind_estimated: row.5,
            in_kind_unvalued: row.6,
        })
    }

    async fn inventory_usage(&self, from: Date, to: Date) -> Result<Inventory> {
        // Movements are instants; a day is the farm's day. Transfers move
        // stock without using it, so they're left out.
        let rows: Vec<(String, String, String, Decimal)> = sqlx::query_as(
            "select m.kind, i.name, i.unit, sum(m.delta) from homestead.stock_movements m
             join homestead.inventory_items i on i.id = m.item_id
             where m.at >= $1::timestamp at time zone $3 and m.at < ($2 + 1)::timestamp at time zone $3
               and m.kind not in ('transfer_in', 'transfer_out')
             group by m.kind, i.id, i.name, i.unit order by i.name, i.unit",
        )
        .bind(from)
        .bind(to)
        .bind(&*self.timezone)
        .fetch_all(&self.pool)
        .await?;
        let mut out =
            Inventory { consumed: vec![], wasted: vec![], received: vec![], harvested_in: vec![], adjusted: vec![] };
        for (kind, key, unit, delta) in rows {
            let (list, quantity) = match kind.as_str() {
                "consumption" => (&mut out.consumed, -delta),
                "waste" => (&mut out.wasted, -delta),
                "receipt" => (&mut out.received, delta),
                "harvest" => (&mut out.harvested_in, delta),
                _ => (&mut out.adjusted, delta),
            };
            list.push(Measure { key, unit, quantity });
        }
        Ok(out)
    }

    async fn cost_totals(&self, from: Date, to: Date) -> Result<Costs> {
        let rows: Vec<(String, Decimal)> = sqlx::query_as(
            "select coalesce(nullif(c.program, ''), p.category), sum(p.amount)
             from homestead.purchases p left join homestead.cost_links c on c.purchase_id = p.id
             where p.purchased_on between $1 and $2 group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let by_program: Vec<Money> = rows.into_iter().map(|(key, amount)| Money { key, amount }).collect();
        Ok(Costs { total: by_program.iter().map(|m| m.amount).sum(), by_program })
    }

    async fn incident_totals(&self, from: Date, to: Date) -> Result<Incidents> {
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "select kind, count(*), count(*) filter (where status = 'open') from homestead.incidents
             where occurred_at >= $1::timestamp at time zone $3 and occurred_at < ($2 + 1)::timestamp at time zone $3
             group by 1 order by 1",
        )
        .bind(from)
        .bind(to)
        .bind(&*self.timezone)
        .fetch_all(&self.pool)
        .await?;
        Ok(Incidents {
            reported: rows.iter().map(|r| r.1).sum(),
            still_open: rows.iter().map(|r| r.2).sum(),
            by_kind: rows.into_iter().map(|(key, count, _)| Count { key, count }).collect(),
        })
    }
}
