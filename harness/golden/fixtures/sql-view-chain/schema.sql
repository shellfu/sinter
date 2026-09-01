-- Raw click events.
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    happened_at TEXT NOT NULL
);

-- Click events only, newest first.
CREATE VIEW click_events AS
SELECT id, happened_at FROM events WHERE kind = 'click';

CREATE MATERIALIZED VIEW recent_events AS
SELECT id, happened_at FROM events;
