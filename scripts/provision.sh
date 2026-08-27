#!/usr/bin/env bash
set -euo pipefail

# One-time git-ark S3 vault provisioning. Run from a trusted admin machine.
#   AWS_PROFILE=your-aws-profile BUCKET=git-ark-vault-<your-account-id> REGION=us-east-1 \
#     PREFIX=git-ark HISTORY_DAYS=90 ./scripts/provision.sh
: "${AWS_PROFILE:?set AWS_PROFILE (e.g. default)}"
: "${BUCKET:?set BUCKET}"
REGION="${REGION:-us-east-1}"
PREFIX="${PREFIX:-git-ark}"
HISTORY_DAYS="${HISTORY_DAYS:-90}"
USER_NAME="${USER_NAME:-git-ark-nas}"

echo ">> creating bucket $BUCKET (Object Lock enabled, versioning on)"
# Object Lock can only be enabled at creation; it requires versioning.
if [ "$REGION" = "us-east-1" ]; then
  aws s3api create-bucket --bucket "$BUCKET" --object-lock-enabled-for-bucket \
    --region "$REGION" >/dev/null 2>&1 || true
else
  aws s3api create-bucket --bucket "$BUCKET" --object-lock-enabled-for-bucket \
    --region "$REGION" --create-bucket-configuration LocationConstraint="$REGION" \
    >/dev/null 2>&1 || true
fi
aws s3api put-bucket-versioning --bucket "$BUCKET" \
  --versioning-configuration Status=Enabled

echo ">> block all public access"
aws s3api put-public-access-block --bucket "$BUCKET" \
  --public-access-block-configuration \
  BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true

echo ">> default SSE (AES256) as defense-in-depth (payloads are already client-side encrypted)"
aws s3api put-bucket-encryption --bucket "$BUCKET" \
  --server-side-encryption-configuration \
  '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'

echo ">> lifecycle: expire history/ after $HISTORY_DAYS days (latest.age is never under history/)"
aws s3api put-bucket-lifecycle-configuration --bucket "$BUCKET" \
  --lifecycle-configuration "{\"Rules\":[{\"ID\":\"expire-history\",\"Status\":\"Enabled\",\"Filter\":{\"Prefix\":\"$PREFIX/\"},\"Expiration\":{\"Days\":$HISTORY_DAYS},\"NoncurrentVersionExpiration\":{\"NoncurrentDays\":$HISTORY_DAYS}}]}"

echo ">> write-only IAM user $USER_NAME (PutObject only)"
aws iam create-user --user-name "$USER_NAME" >/dev/null 2>&1 || true
aws iam put-user-policy --user-name "$USER_NAME" --policy-name git-ark-put-only \
  --policy-document "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Action\":\"s3:PutObject\",\"Resource\":\"arn:aws:s3:::$BUCKET/$PREFIX/*\"}]}"

echo ">> creating access key for $USER_NAME (paste into secrets.toml on the host, then delete this output)"
aws iam create-access-key --user-name "$USER_NAME" \
  --query 'AccessKey.{id:AccessKeyId,secret:SecretAccessKey}' --output table

echo ">> done. Vault: s3://$BUCKET/$PREFIX/"
