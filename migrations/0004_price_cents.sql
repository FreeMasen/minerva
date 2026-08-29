-- Money moves from a lossy REAL dollar amount to an exact integer number of
-- cents (minor units). The domain layer models this as UsdCents(u32) inside an
-- Acquisition enum; the DB just stores the integer.

ALTER TABLE books ADD COLUMN price_cents INTEGER;

UPDATE books
    SET price_cents = CAST(ROUND(price_usd * 100) AS INTEGER)
    WHERE price_usd IS NOT NULL;

ALTER TABLE books DROP COLUMN price_usd;
