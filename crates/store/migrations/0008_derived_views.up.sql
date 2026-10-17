-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-001 T-001-05: views derived from durable records only (NFR-7, REQ-ST-7).
--
-- The restore drill compares these between a database and its restored copy. They read nothing
-- but the tables, so a restore that kept every record keeps every view.

-- Each transfer's state as recorded, beside the state re-derived from its transitions.
CREATE VIEW transfer_status AS
SELECT
    t.bridge_reference,
    t.request_reference,
    t.kind,
    t.asset,
    t.amount,
    t.state,
    t.condition,
    COALESCE(last.to_state, t.state) AS derived_state,
    COALESCE(last.n, 0)              AS transitions,
    t.source_final_block_number,
    t.source_final_block_hash,
    t.source_final_block_time,
    t.destination_included_block_number,
    t.destination_included_block_hash,
    t.destination_included_block_time,
    t.destination_final_block_number,
    t.destination_final_block_hash,
    t.destination_final_block_time
FROM transfers t
LEFT JOIN LATERAL (
    SELECT
        (SELECT to_state FROM transitions WHERE transfer_id = t.id ORDER BY id DESC LIMIT 1) AS to_state,
        count(*) AS n
    FROM transitions
    WHERE transfer_id = t.id
) last ON last.n > 0;

-- The reserve book's balance per (asset, account, network): debits minus credits.
CREATE VIEW book_balances AS
SELECT
    asset,
    account,
    network,
    sum(CASE side WHEN 'debit' THEN amount ELSE -amount END) AS balance,
    count(*)                                                AS postings
FROM book_postings
GROUP BY asset, account, network;

-- The event log's head: the last position, the count, and the position the head row holds.
CREATE VIEW event_log_position AS
SELECT
    (SELECT seq FROM event_log_head)          AS head,
    (SELECT COALESCE(max(seq), 0) FROM event_log) AS last_seq,
    (SELECT count(*) FROM event_log)          AS entries;
