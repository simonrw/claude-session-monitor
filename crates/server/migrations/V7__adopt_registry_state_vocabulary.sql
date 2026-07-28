-- PRO-214: adopt the Claude Code session registry's own state vocabulary
-- (busy, shell, idle, waiting, ended) in place of the old two-state
-- Working/Waiting model and its Permission/Input WaitingReason split.
--
-- Existing rows:
--   * 'working' -> 'busy'. status_tool, if set, is left untouched: the new
--     Busy variant carries the same optional tool field the old Working
--     variant did.
--   * 'waiting' -> unchanged (still 'waiting'). waiting_detail is left
--     untouched.
--   * 'ended' -> unchanged.
--
-- No existing row can already be 'shell' or 'idle' - those are new
-- vocabulary this migration introduces to the schema, written only by
-- csm-watcher (see common::session::Status::from_registry) from here on.
UPDATE sessions SET status = 'busy' WHERE status = 'working';

-- waiting_reason ('permission'/'input') has no equivalent in the new model:
-- the registry carries no structured signal distinguishing a permission
-- prompt from any other kind of pause, so that distinction was never more
-- than a guess. Dropping the column loses nothing waiting_detail didn't
-- already carry as the only structured detail associated with either
-- reason.
ALTER TABLE sessions DROP COLUMN waiting_reason;
