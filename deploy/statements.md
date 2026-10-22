<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# The daily statement bucket

The core writes each daily signed statement, and its signature file, to an S3 bucket itself
(`REQ-PR-1`, `REQ-PR-2`; PLAN `DEC-16`; issue #58). Nothing copies files, and nothing else writes
to the bucket. This page says what the bucket must be, how the core is configured, and the policy
its task role needs. Account ids, bucket names and key ids below are placeholders.

## Configuration

`[statement.s3]` in the core's configuration, beside the signer settings of `[statement]`. Exactly
one of `statement.directory` and `[statement.s3]` is set. `directory` (the file sink) and
`object_lock_mode = "off"` are refused unless `environment = "test"`.

```toml
[statement]
signer_socket = "/run/bezent-signer/signer.sock"
signer_uid = 65533
signer_token_file = "/run/secrets/bezent-core-signer-token"
public_key = "0x<the statement key's ed25519 public key>"

[statement.s3]
bucket = "example-bezent-statements"   # created as described below
prefix = "statements"                  # optional; plain path segments, no slashes at the ends
region = "us-east-2"
# kms_key_id = "alias/example-bezent-statements"   # optional SSE-KMS; see "Encryption"
object_lock_mode = "GOVERNANCE"        # GOVERNANCE | COMPLIANCE | off (off: tests only)
retention_days = 365                   # 1 to 36500; left out, 365. The mode has no default
```

No credential is configured. The core takes them from its ECS task role only, never the instance
role, and refuses to start without one (`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`) or when
`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_PROFILE` or
`AWS_SHARED_CREDENTIALS_FILE` is set (`REQ-KY-1`).

## Bucket requirements

- **Versioning and Object Lock are enabled when the bucket is created.** Object Lock cannot be
  turned on for an existing bucket without AWS support, and it requires versioning.
- Choose `COMPLIANCE` for production if the retention must be unbreakable by anyone, the account's
  root included; `GOVERNANCE` lets a principal holding `s3:BypassGovernanceRetention` shorten it.
  Neither is granted to the core. A retention of `retention_days` is set on every object, counted
  from the write.
- A bucket policy that denies `s3:DeleteObject`, `s3:DeleteObjectVersion`, `s3:PutObjectRetention`
  with a shorter date, and `s3:PutBucketObjectLockConfiguration` to every principal but a named
  break-glass role is advised. It is the deployment's, not the core's.
- The statements are public by design (`DEC-16`: anyone can detect a later revision without
  bezent). Serving them to the public (a bucket policy for `s3:GetObject` on the prefix, or a CDN
  in front) is the deployment's choice; the core needs none of it.

## Keys

`<prefix>/<YYYY>/<MM>/<DD>/statement-<asset>-<YYYY-MM-DD>.json`, and the signature file beside it
with `.sig` appended. The date is the statement's own `issued_at` in UTC. The core writes with
`If-None-Match: *`, so a key that exists is never overwritten; the same bytes written again are
recognised as the write that already landed, and different bytes are refused and paged.

## The task role's policy

The role of the core's task, and no other task, gets this. The core writes statements and reads
them back; it cannot delete, change retention or bypass governance.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "WriteStatements",
      "Effect": "Allow",
      "Action": ["s3:PutObject", "s3:PutObjectRetention"],
      "Resource": "arn:aws:s3:::example-bezent-statements/statements/*"
    },
    {
      "Sid": "ReadStatementsBack",
      "Effect": "Allow",
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::example-bezent-statements/statements/*"
    },
    {
      "Sid": "TellMissingFromForbidden",
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::example-bezent-statements",
      "Condition": { "StringLike": { "s3:prefix": "statements/*" } }
    },
    {
      "Sid": "StatementKey",
      "Effect": "Allow",
      "Action": ["kms:GenerateDataKey", "kms:Encrypt", "kms:Decrypt"],
      "Resource": "arn:aws:kms:us-east-2:111122223333:key/<key id>"
    }
  ]
}
```

- Leave out `StatementKey` when `kms_key_id` is not set and the bucket's default encryption is SSE-S3.
- `s3:GetObject` and `s3:ListBucket` are there because the core reads back what it wrote: before a
  statement is written it asks whether the place's object already exists (a write that landed and
  was not recorded is adopted, never replaced), and after a refused overwrite it compares bytes.
  Without `s3:ListBucket`, S3 answers a missing key with `403`, not `404`, and no first statement
  could be written. The `s3:prefix` condition keeps the listing to the statements' prefix. The
  statements are public, so this reveals nothing.
- The core never needs `s3:DeleteObject*`, `s3:BypassGovernanceRetention`,
  `s3:PutBucketObjectLockConfiguration`, or any bucket-level action.
- If the key is customer-managed, its key policy allows the task role the same three actions.

## Encryption

With `kms_key_id`, objects are SSE-KMS. Anonymous readers cannot read SSE-KMS objects, so a public
bucket and `kms_key_id` do not go together unless a reader with the key (a CDN's origin identity)
serves them. Without it, the bucket's default encryption applies (SSE-S3 keeps the bucket
publicly readable).

## What the core does when it cannot write

- It retries transient failures (throttling, `409`, `5xx`, timeouts) three times (3 s each, 0.5 s
  then 1 s apart) and again every cycle after that. The 60 s statement budget of a cycle covers all
  assets together: a slow bucket can leave later assets to the next cycle, and `statement-overdue`
  (an hour after a statement fell due) is the backstop.
- The signed statement is kept (the signer's intent is in the store); the same bytes are written
  when the bucket answers again, even on a later UTC day.
- It pages `statement-publish-failed` at once, once a day while it persists, and
  `statement-overdue` an hour after the statement fell due. A publication that fails is never
  skipped.
- It records the key, ETag and version id of both objects in `reserve_statement_objects` only
  after both were written.
