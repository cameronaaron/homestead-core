//! The last values someone used on Today, so the next entry can start there.

use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{Result, Store};

#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct FieldMemory {
    pub location_id: Option<Uuid>,
    pub planting_id: Option<Uuid>,
    pub harvest_quantity: Option<Decimal>,
    pub harvest_unit: Option<String>,
    pub animal_id: Option<Uuid>,
}

impl Store {
    pub async fn field_memory(&self, actor_id: Uuid) -> Result<FieldMemory> {
        let row: Option<FieldMemory> = sqlx::query_as(
            "select location_id, planting_id, harvest_quantity, harvest_unit, animal_id
             from homestead.field_defaults where actor_id = $1",
        )
        .bind(actor_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.unwrap_or_default())
    }

    pub async fn remember_water(&self, actor_id: Uuid, location_id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.field_defaults (actor_id, location_id) values ($1, $2)
             on conflict (actor_id) do update set location_id = excluded.location_id",
        )
        .bind(actor_id)
        .bind(location_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn remember_harvest(
        &self,
        actor_id: Uuid,
        planting_id: Uuid,
        quantity: Decimal,
        unit: &str,
    ) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.field_defaults (actor_id, planting_id, harvest_quantity, harvest_unit)
             values ($1, $2, $3, $4)
             on conflict (actor_id) do update set
               planting_id = excluded.planting_id,
               harvest_quantity = excluded.harvest_quantity,
               harvest_unit = excluded.harvest_unit",
        )
        .bind(actor_id)
        .bind(planting_id)
        .bind(quantity)
        .bind(unit)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn remember_eggs(&self, actor_id: Uuid, animal_id: Uuid) -> Result<()> {
        let mut tx = self.begin().await?;
        sqlx::query(
            "insert into homestead.field_defaults (actor_id, animal_id) values ($1, $2)
             on conflict (actor_id) do update set animal_id = excluded.animal_id",
        )
        .bind(actor_id)
        .bind(animal_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
