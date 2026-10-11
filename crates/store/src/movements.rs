//! Moves of animals and equipment between places. The database writes a
//! movement whenever a current place changes; `move_to` only adds the
//! time and note a person gave.

use homestead_domain::movements::{Movement, Mover, NewMove};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Result, Store, StoreError};

macro_rules! movement_cols {
    () => {
        "m.id, m.moved_at, m.animal_id, m.equipment_id, coalesce(a.name, e.name),
         m.from_location_id, f.name, m.to_location_id, t.name, m.note
         from homestead.movements m
         left join homestead.animals a on a.id = m.animal_id
         left join homestead.equipment e on e.id = m.equipment_id
         left join homestead.locations f on f.id = m.from_location_id
         left join homestead.locations t on t.id = m.to_location_id"
    };
}

type MovementRow = (
    Uuid,
    OffsetDateTime,
    Option<Uuid>,
    Option<Uuid>,
    String,
    Option<Uuid>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    String,
);

fn movement(r: MovementRow) -> Result<Movement> {
    let (id, moved_at, animal, equipment, mover_name, from, from_name, to, to_name, note) = r;
    let mover = match (animal, equipment) {
        (Some(a), None) => Mover::Animal(a),
        (None, Some(e)) => Mover::Equipment(e),
        _ => return Err(StoreError::Corrupt(format!("movement {id} moves more than one thing"))),
    };
    Ok(Movement { id, moved_at, mover, mover_name, from: from.zip(from_name), to: to.zip(to_name), note })
}

impl Store {
    /// Moves an animal or a piece of equipment, at `at` if it happened
    /// earlier. Moving somewhere it already is does nothing.
    pub async fn move_to(&self, mover: Mover, input: NewMove) -> Result<()> {
        let m = input.validated()?;
        if let Some(to) = m.to {
            let _ = self.get_location(to).await?;
        }
        let mut tx = self.begin().await?;
        sqlx::query("select set_config('homestead.moved_at', $1, true), set_config('homestead.move_note', $2, true)")
            .bind(m.at.map(|t| t.format(&time::format_description::well_known::Rfc3339).expect("formats")))
            .bind(&m.note)
            .execute(&mut *tx)
            .await?;
        let (sql, id, what) = match mover {
            Mover::Animal(id) => ("update homestead.animals set home_location_id = $2 where id = $1", id, "animal"),
            Mover::Equipment(id) => ("update homestead.equipment set location_id = $2 where id = $1", id, "equipment"),
        };
        let n = sqlx::query(sql).bind(id).bind(m.to).execute(&mut *tx).await?.rows_affected();
        if n == 0 {
            return Err(StoreError::NotFound(what));
        }
        tx.commit().await?;
        Ok(())
    }

    /// Everywhere it has been, newest first.
    pub async fn movements_of(&self, mover: Mover) -> Result<Vec<Movement>> {
        let (animal, equipment) = match mover {
            Mover::Animal(id) => (Some(id), None),
            Mover::Equipment(id) => (None, Some(id)),
        };
        let rows: Vec<MovementRow> = sqlx::query_as(concat!(
            "select ",
            movement_cols!(),
            " where m.animal_id is not distinct from $1 and m.equipment_id is not distinct from $2
               and ($1::uuid is not null or $2::uuid is not null)
             order by m.moved_at desc, m.recorded_at desc"
        ))
        .bind(animal)
        .bind(equipment)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(movement).collect()
    }

    /// What came into or left a place (or anywhere inside it), newest
    /// first; animals only with `animals`, equipment only with `equipment`.
    pub async fn movements_at(
        &self,
        location_id: Uuid,
        animals: bool,
        equipment: bool,
        limit: i64,
    ) -> Result<Vec<Movement>> {
        let rows: Vec<MovementRow> = sqlx::query_as(concat!(
            "with recursive place as (
               select id from homestead.locations where id = $1
               union all select c.id from homestead.locations c join place on c.parent_id = place.id
             ) select ",
            movement_cols!(),
            " where (m.to_location_id in (select id from place) or m.from_location_id in (select id from place))
               and (($2 and m.animal_id is not null) or ($3 and m.equipment_id is not null))
             order by m.moved_at desc, m.recorded_at desc limit $4"
        ))
        .bind(location_id)
        .bind(animals)
        .bind(equipment)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(movement).collect()
    }
}
