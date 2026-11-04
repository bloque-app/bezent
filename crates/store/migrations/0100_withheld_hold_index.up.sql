-- SPDX-License-Identifier: GPL-3.0-or-later
-- Review of the fix round of #35: the core reads the last page of a hold (`withheld`,
-- services/core/src/engine.rs) from the event log on every cycle of every transfer that waits on
-- an inclusion: by kind, transfer and the hold named in the payload. The log has no index but its
-- position, so each read scanned it. A partial index on that one kind serves the read and nothing
-- else; it holds one entry per page, a handful a day.
-- Additive: an index only. No privilege changes, no data touched.
--
-- Deploying it: this is a plain `CREATE INDEX`, which takes a SHARE lock on `event_log` for the whole
-- scan of the table, so every writer to the log (all of the services) waits until it ends.
-- `CREATE INDEX CONCURRENTLY` would not block them, but it cannot run inside the transaction the
-- migrator wraps each migration in. The migrator requires the services stopped (it gives up
-- with "stop the services and end any idle transaction on the store" when a lock it needs times
-- out), so the lock blocks nobody on a normal run; but on a log that has grown large, the build time
-- is time the bridge is down.
CREATE INDEX event_log_blocked_by_inclusion
    ON event_log (transfer_id, (payload ->> 'hold'), seq DESC)
    WHERE kind = 'resolution-blocked-by-inclusion';
