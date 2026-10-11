//! Places' shapes on the farm plan, and what the map says about each.

use homestead_domain::LocationKind;
use homestead_domain::map::{Pt, Shape};
use sqlx::types::Json;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{Result, Store, StoreError, parse};

/// A place on the map and, counting the places inside it, what's going on
/// there: enough to decide whether to open its page.
#[derive(Debug, Clone, PartialEq)]
pub struct MapPlace {
    pub id: Uuid,
    pub name: String,
    pub kind: LocationKind,
    pub parent_id: Option<Uuid>,
    /// How many places it sits inside; outer places are drawn first.
    pub depth: i32,
    pub shape: Shape,
    /// Crops growing or being harvested, alphabetical.
    pub growing: Vec<String>,
    /// The soonest expected harvest from today on.
    pub next_harvest: Option<Date>,
    pub last_watered: Option<OffsetDateTime>,
    pub open_tasks: i64,
    /// Crop health observations not yet resolved.
    pub open_issues: i64,
}

type MapRow = (
    Uuid,
    String,
    String,
    Option<Uuid>,
    i32,
    String,
    Json<Vec<Pt>>,
    Vec<String>,
    Option<Date>,
    Option<OffsetDateTime>,
    i64,
    i64,
);

fn shape(kind: &str, points: Vec<Pt>) -> Result<Shape> {
    Shape { kind: parse(kind)?, points }.validated().map_err(|e| StoreError::Corrupt(e.to_string()))
}

impl Store {
    /// Draws a place on the plan, or with `None` takes it off.
    pub async fn set_location_shape(&self, location_id: Uuid, s: Option<Shape>) -> Result<()> {
        let _ = self.get_location(location_id).await?;
        let mut tx = self.begin().await?;
        match s {
            Some(s) => {
                let s = s.validated()?;
                sqlx::query(
                    "insert into homestead.location_shapes (location_id, kind, points) values ($1, $2, $3)
                     on conflict (location_id) do update set kind = excluded.kind, points = excluded.points,
                       updated_at = now()",
                )
                .bind(location_id)
                .bind(s.kind.as_str())
                .bind(Json(&s.points))
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query("delete from homestead.location_shapes where location_id = $1")
                    .bind(location_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn location_shape(&self, location_id: Uuid) -> Result<Option<Shape>> {
        let row: Option<(String, Json<Vec<Pt>>)> =
            sqlx::query_as("select kind, points from homestead.location_shapes where location_id = $1")
                .bind(location_id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|(k, p)| shape(&k, p.0)).transpose()
    }

    /// Every place in use that has a shape, outermost first, each with a
    /// summary of itself and the places inside it.
    pub async fn farm_map(&self) -> Result<Vec<MapPlace>> {
        let today = self.today().await?;
        let rows: Vec<MapRow> = sqlx::query_as(
            "with recursive
               depth as (
                 select id, 0 as depth from homestead.locations where parent_id is null
                 union all select c.id, d.depth + 1 from homestead.locations c join depth d on c.parent_id = d.id
               ),
               tree as (
                 select location_id as root, location_id as id from homestead.location_shapes
                 union all select t.root, c.id from homestead.locations c join tree t on c.parent_id = t.id
               )
             select l.id, l.name, l.kind, l.parent_id, d.depth, s.kind, s.points,
               coalesce((select array_agg(distinct p.crop order by p.crop) from homestead.plantings p
                 where p.status in ('growing', 'harvesting')
                   and p.location_id in (select id from tree where root = l.id)), '{}'),
               (select min(coalesce(p.expected_harvest_on, p.expected_harvest_until)) from homestead.plantings p
                 where p.status in ('planned', 'growing', 'harvesting')
                   and coalesce(p.expected_harvest_on, p.expected_harvest_until) >= $1
                   and p.location_id in (select id from tree where root = l.id)),
               (select max(r.started_at) from homestead.irrigation_records r
                 where r.location_id in (select id from tree where root = l.id)),
               (select count(*) from homestead.tasks t
                 where t.status = 'open' and t.location_id in (select id from tree where root = l.id)),
               (select count(*) from homestead.crop_observations o
                 left join homestead.plantings p on p.id = o.planting_id
                 where o.resolved_on is null
                   and coalesce(o.location_id, p.location_id) in (select id from tree where root = l.id))
             from homestead.location_shapes s
             join homestead.locations l on l.id = s.location_id
             join depth d on d.id = l.id
             where l.active
             order by d.depth, l.name",
        )
        .bind(today)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, name, kind, parent_id, depth, sk, pts, growing, next_harvest, last_watered, tasks, issues)| {
                Ok(MapPlace {
                    id,
                    name,
                    kind: parse(&kind)?,
                    parent_id,
                    depth,
                    shape: shape(&sk, pts.0)?,
                    growing,
                    next_harvest,
                    last_watered,
                    open_tasks: tasks,
                    open_issues: issues,
                })
            })
            .collect()
    }
}
