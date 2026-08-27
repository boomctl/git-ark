# Provisioning the S3 vault

One-time setup of the S3 bucket and write-only IAM identity that `git-ark`
backs up to. Run this **once**, from a trusted admin machine — it needs an AWS
profile with permission to create buckets, bucket policies, and IAM users,
which the host itself never has.

## Prerequisites

- The [`aws` CLI](https://aws.amazon.com/cli/), configured with a profile that
  has admin (or bucket + IAM) permissions on the target account.
- A globally-unique bucket name. S3 bucket names are a single global
  namespace, so `<project>-<account-id>` is a reliable pattern.

## Reference values

The values below are examples; substitute your own throughout.

| Variable | Example value |
|---|---|
| `AWS_PROFILE` | `<your-aws-profile>` |
| Account | `<your-account-id>` |
| `REGION` | `us-east-1` |
| `BUCKET` | `git-ark-vault-<your-account-id>` |
| `PREFIX` | `git-ark` |
| `HISTORY_DAYS` | `90` |
| `USER_NAME` | `git-ark-nas` |

## Run it

```bash
AWS_PROFILE=<your-aws-profile> BUCKET=git-ark-vault-<your-account-id> REGION=us-east-1 \
  PREFIX=git-ark HISTORY_DAYS=90 ./scripts/provision.sh
```

`AWS_PROFILE` and `BUCKET` are required; everything else has the defaults
shown above. The script is idempotent-ish — re-running it is safe (bucket and
IAM-user creation calls tolerate "already exists" and everything else is a
plain `put-*` that just re-applies the same configuration). The one exception
is `create-access-key`: re-running the script mints a **new** key each time
without revoking the old one, so only re-run the whole script if you actually
want an additional key.

The script prints the new access key ID and secret to the terminal at the end.
Paste those into the host's `secrets.toml` (see [`docs/deploy.md`](deploy.md))
under `[aws]`, then clear your terminal scrollback — that's the only place the
secret is ever displayed.

## What it creates

- **The bucket**, with Object Lock **capability** enabled and versioning
  **on**. Object Lock can only be turned on at bucket creation, which is why
  it's requested up front even though no retention *lock* is applied yet (see
  below). Versioning is a requirement of Object Lock and also protects
  against an accidental overwrite of `latest.age`.
- **Public access block**, all four settings on — nothing in this bucket is
  ever reachable without the account's own IAM credentials.
- **Default server-side encryption** (SSE-S3 / AES256), as defense-in-depth.
  This is on top of, not instead of, the client-side age encryption the host
  applies before upload — S3 never sees plaintext.
- **A lifecycle rule** (`expire-history`) that expires objects under
  `<prefix>/` (and their noncurrent versions) after `HISTORY_DAYS` days. This
  is how retention is actually enforced, since the vault has no delete
  permission of its own: `latest.age` is overwritten every push, so its
  *current* version is always fresh, while old `history/<ts>.age` snapshots
  age out on schedule.
- **A write-only IAM user** (`git-ark-nas` by default) with an inline policy
  granting `s3:PutObject` — and only that — on
  `arn:aws:s3:::<bucket>/<prefix>/*`. No `GetObject`, `ListBucket`, or
  `DeleteObject`. A host that's fully compromised can still only add new
  objects to the vault; it can't read, enumerate, or destroy backups.
- **An access key** for that user, printed once at the end of the run.

## Rotating the access key

The host holds a long-lived key with no expiry, so plan to rotate it
periodically or after any suspected compromise:

```bash
# create a second key alongside the existing one
aws --profile <your-aws-profile> iam create-access-key --user-name git-ark-nas

# update secrets.toml on the host with the new key, verify a push still
# backs up successfully, then deactivate/delete the old one
aws --profile <your-aws-profile> iam update-access-key --user-name git-ark-nas \
  --access-key-id <OLD_KEY_ID> --status Inactive
aws --profile <your-aws-profile> iam delete-access-key --user-name git-ark-nas \
  --access-key-id <OLD_KEY_ID>
```

IAM allows at most two access keys per user, so create-then-cutover-then-delete
(rather than delete-then-create) avoids a window with no working key.

## Enabling retention lock later

The bucket is created with Object Lock **capability** but no retention lock
is set — objects can still be deleted with a sufficiently-privileged
identity (though `git-ark-nas` itself can never do so). To later lock history
objects against deletion for a fixed period, even by an admin, set a default
retention configuration:

```bash
aws --profile <your-aws-profile> s3api put-object-lock-configuration \
  --bucket git-ark-vault-<your-account-id> \
  --object-lock-configuration '{
    "ObjectLockEnabled": "Enabled",
    "Rule": {
      "DefaultRetention": {
        "Mode": "GOVERNANCE",
        "Days": 90
      }
    }
  }'
```

Use `GOVERNANCE` mode if you want the option to override retention later with
elevated permissions (e.g. to fix a mistake), or `COMPLIANCE` mode if you want
retention to be unoverridable by anyone, including the root account, until it
expires. This only applies to objects uploaded *after* the configuration is
set — it is not retroactive. If you turn this on, keep `HISTORY_DAYS` (the
lifecycle expiration) at least as long as the retention `Days`, or the
lifecycle rule will simply be blocked from deleting locked objects until the
lock expires.

## Verifying the setup

```bash
aws --profile <your-aws-profile> s3api get-bucket-versioning --bucket git-ark-vault-<your-account-id>
aws --profile <your-aws-profile> s3api get-public-access-block --bucket git-ark-vault-<your-account-id>
aws --profile <your-aws-profile> s3api get-bucket-lifecycle-configuration --bucket git-ark-vault-<your-account-id>
aws --profile <your-aws-profile> iam get-user-policy --user-name git-ark-nas --policy-name git-ark-put-only
```

The policy document returned by the last command should show `s3:PutObject`
as the only allowed action.
