ALTER TABLE operations
ADD COLUMN next_attempt_at timestamptz NOT NULL DEFAULT now();

CREATE INDEX operations_retry_worker_idx
ON operations(worker_id, next_attempt_at, created_at)
WHERE status = 'pending';
