SELECT u.name, o.total
FROM users u
JOIN orders o ON o.user_id = u.id;

INSERT INTO orders (id, user_id, total) VALUES (1, 2, 300);

UPDATE users SET name = 'renamed' WHERE id = 1;

DELETE FROM orders WHERE id = 2;

SELECT * FROM audit_log;
