-- SPDX-License-Identifier: GPL-3.0-or-later
-- Review of the fix round of #35: the core reads the last page of a hold (`withheld`,
-- services/core/src/engine.rs) from the event log on every cycle of every transfer that waits on
-- an inclusion: by kind, transfer and the hold named in the payload. The log has no index but its
-- position, so each read scanned it. A partial index on that one kind serves the read and nothing
-- else; it holds one entry per page, a handful a day.
-- Additive: an index only. No privilege changes, no data touched.
CREATE INDEX event_log_blocked_by_inclusion
    ON event_log (transfer_id, (payload ->> 'hold'), seq DESC)
    WHERE kind = 'resolution-blocked-by-inclusion';
