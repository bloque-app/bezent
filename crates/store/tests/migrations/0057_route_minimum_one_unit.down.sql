-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): routes may again be declared below one unit of the asset.
DROP TRIGGER routes_minimum_one_unit ON routes;
DROP TRIGGER admitted_tokens_routes_one_unit ON admitted_tokens;
DROP FUNCTION route_minimum_on_route();
DROP FUNCTION route_minimum_is_one_unit();
