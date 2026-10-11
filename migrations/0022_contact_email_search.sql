-- Find a contact by the start of any part of their email ("ana@exa").
--
-- PostgreSQL's parser reads a whole address as one `email` token, but a
-- partial one ("ana@exa" has no dot in its domain) as the words "ana" and
-- "exa", so to_tsquery('simple', 'ana@exa:*') becomes 'ana':* <-> 'exa':*.
-- 0019 indexed only the whole-address token, so no lexeme began with "exa"
-- and partial addresses never matched. Also indexing the address split at
-- its punctuation gives those words, adjacent, as the phrase needs. The
-- whole-address token stays, so a complete address still matches as one.
alter table homestead.contacts drop column search;
alter table homestead.contacts add column search tsvector generated always as (
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('simple', email), 'B') ||
    setweight(to_tsvector('simple', translate(email, '@.+_-', '     ')), 'B')) stored;
create index contacts_search on homestead.contacts using gin (search);
