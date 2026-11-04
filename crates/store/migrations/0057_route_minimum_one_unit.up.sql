-- SPDX-License-Identifier: GPL-3.0-or-later
-- M-2 (issue #30), REQ-LM-1, REQ-AS-9: a route's minimum is at least one minor unit of the bridged
-- asset, expressed in the origin token, that is the declared ratio.
--
-- 0010 only required `minimum >= 1`. For an asset whose origin token has more precision (bBTC: 8
-- against BTCB's 18, a ratio of 10^10) a route minimum under the ratio let a deposit through the
-- request check (ERR-BelowMinimum compares the amount to the route's minimum) whose mint is zero:
-- the core ordered a mint of nothing, the transfer sat in Source-Final for good, and the arrival's
-- remainder stayed in owed mint, never in surplus (found by the M-2 boundary tests). The declaration
-- is now refused when it breaks the rule, whichever is written last, the route or the token's
-- precision. Declarations are the migration owner's; this only refuses a wrong one. Additive: two
-- trigger functions and their triggers; no row is touched. A deployment that already holds a route
-- below its ratio must correct it before this runs (the migration fails naming the route).

CREATE FUNCTION route_minimum_is_one_unit() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    r RECORD;
BEGIN
    FOR r IN
        SELECT ro.asset, ro.network, ro.direction, ro.minimum,
               power(10::numeric, k.precision - a.precision) AS ratio
        FROM routes ro
        JOIN assets a ON a.asset = ro.asset
        JOIN admitted_tokens k ON k.asset = ro.asset AND k.network = ro.network
        WHERE ro.asset = NEW.asset AND ro.network = NEW.network
    LOOP
        IF r.minimum < r.ratio THEN
            RAISE EXCEPTION 'route % % % refused: its minimum % is under one minor unit of the bridged asset, % units of the origin token (REQ-LM-1, REQ-AS-9)',
                r.asset, r.network, r.direction, r.minimum, r.ratio
                USING ERRCODE = 'BZ001';
        END IF;
    END LOOP;
    RETURN NEW;
END
$$;

-- A route written after its token is declared: checked on the new values as written.
CREATE FUNCTION route_minimum_on_route() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    ratio NUMERIC;
BEGIN
    SELECT power(10::numeric, k.precision - a.precision) INTO ratio
    FROM assets a JOIN admitted_tokens k ON k.asset = a.asset
    WHERE a.asset = NEW.asset AND k.network = NEW.network;
    IF ratio IS NOT NULL AND NEW.minimum < ratio THEN
        RAISE EXCEPTION 'route % % % refused: its minimum % is under one minor unit of the bridged asset, % units of the origin token (REQ-LM-1, REQ-AS-9)',
            NEW.asset, NEW.network, NEW.direction, NEW.minimum, ratio
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER routes_minimum_one_unit
    BEFORE INSERT OR UPDATE OF minimum, asset, network ON routes
    FOR EACH ROW EXECUTE FUNCTION route_minimum_on_route();
CREATE TRIGGER admitted_tokens_routes_one_unit
    AFTER INSERT OR UPDATE OF precision, asset, network ON admitted_tokens
    FOR EACH ROW EXECUTE FUNCTION route_minimum_is_one_unit();

REVOKE EXECUTE ON FUNCTION route_minimum_is_one_unit(), route_minimum_on_route() FROM PUBLIC;

-- A route already below its ratio stops this migration, naming it.
DO $$
DECLARE
    bad TEXT;
BEGIN
    SELECT string_agg(ro.asset || ' ' || ro.network || ' ' || ro.direction, ', ') INTO bad
    FROM routes ro
    JOIN assets a ON a.asset = ro.asset
    JOIN admitted_tokens k ON k.asset = ro.asset AND k.network = ro.network
    WHERE ro.minimum < power(10::numeric, k.precision - a.precision);
    IF bad IS NOT NULL THEN
        RAISE EXCEPTION 'routes under one minor unit of their bridged asset: %; correct them first (REQ-LM-1)', bad
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;
