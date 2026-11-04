-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reverts 0062_reversed_arrivals, in tests only.

DROP TRIGGER transfers_refuse_reversed_source ON transfers;
DROP FUNCTION transfers_refuse_reversed_source();
DROP FUNCTION attested_arrival_reversed(TEXT, BIGINT, BYTEA);
