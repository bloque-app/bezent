-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): review M3 of PR #139, the declaration of directing code
-- enforced, back to 0147's.
DROP TRIGGER transfers_guard_directing_declared ON transfers;
DROP FUNCTION transfers_guard_directing_declared();

CREATE OR REPLACE FUNCTION directed_return(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SET search_path = public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM transfers t
        JOIN observations o
          ON o.network = t.source_network AND o.block_number = t.source_block_number
         AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
         AND o.position_index = t.source_position_index
        WHERE t.id = p_transfer AND t.kind = 'refund' AND t.refund_of IS NULL
          AND o.kind = 'directed_arrival')
$$;

DROP FUNCTION directing_declared(TEXT, TEXT, TEXT, BIGINT);
