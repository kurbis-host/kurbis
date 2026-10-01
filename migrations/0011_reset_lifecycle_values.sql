ALTER TYPE server_observed_state ADD VALUE IF NOT EXISTS 'resetting';
ALTER TYPE operation_kind ADD VALUE IF NOT EXISTS 'reset';
ALTER TYPE snapshot_reason ADD VALUE IF NOT EXISTS 'reset';
