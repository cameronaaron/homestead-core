-- How an animal is identified, and whose name its microchip comes back to.
--
-- Microchips are stored normalised (no spaces or dashes, upper case): 15
-- digits for ISO 11784/11785 chips (134.2 kHz, today's standard), or 9 to
-- 10 characters for the older 125 kHz chips (AVID, FECAVA/FDX-A) still in
-- older cats. A number is one animal: a scanner reading a chip must land on
-- exactly one record. Production had no animals when this was added
-- (checked read-only 2026-09-29), so nothing needed cleaning.

alter table homestead.animals
  add constraint animals_microchip_format
    check (microchip = '' or microchip ~ '^([0-9]{15}|[0-9A-F]{9,10})$'),
  -- The registry the chip is enrolled with (Found.org, 24Petwatch, …).
  add column chip_registry text not null default '' check (length(chip_registry) <= 80),
  -- Whose contact details a vet or shelter gets when they scan it.
  add column chip_registered_to text not null default 'nobody'
    check (chip_registered_to in ('nobody', 'us', 'adopter', 'other')),
  -- A leg band, ear tag or collar colour: how a bird or goat is told apart.
  add column band text not null default '' check (length(band) <= 40);

create unique index animals_microchip_unique on homestead.animals (microchip) where microchip <> '';
