-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reverts 0060_attested_backend, in tests only.

DROP TRIGGER intents_guard_attested ON intents;
DROP FUNCTION intents_guard_attested();
DROP FUNCTION bezent_record_attested_observation(BIGINT);
DROP TABLE attested_reports;
DROP FUNCTION attested_reports_guard();
DROP TABLE attesting_keys;
DROP TABLE attesting_parties;
DROP FUNCTION attesting_parties_guard();

ALTER TABLE intake DROP CONSTRAINT intake_kind_check;
ALTER TABLE intake ADD CONSTRAINT intake_kind_check
    CHECK (kind IN ('deposit', 'withdrawal', 'refund'));
