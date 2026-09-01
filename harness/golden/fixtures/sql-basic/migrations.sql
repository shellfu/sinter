ALTER TABLE users ADD COLUMN email TEXT;
DROP TABLE orders;
DROP INDEX orders_user_id_idx;
