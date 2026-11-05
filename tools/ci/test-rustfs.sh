#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

# Keep local and CI runs on the same image. No host data or credentials are mounted.
image=rustfs/rustfs@sha256:ed9210d6411d77ffceb70d94b964a3cb7d225983368bc25bebe01e995293dcc2
name="ippai-rustfs-${GITHUB_RUN_ID:-local}-$$"
export AWS_ACCESS_KEY_ID=ippai-ci-access
export AWS_SECRET_ACCESS_KEY=ippai-ci-secret-only-for-tests
export AWS_EC2_METADATA_DISABLED=true
cleanup() {
    status=$?
    if [ "$status" -ne 0 ]; then docker logs "$name" >&2 || true; fi
    docker rm -fv "$name" >/dev/null 2>&1 || true
    exit "$status"
}
trap cleanup EXIT
docker run -d --name "$name" -p 127.0.0.1::9000 \
    -e RUSTFS_ACCESS_KEY="$AWS_ACCESS_KEY_ID" \
    -e RUSTFS_SECRET_KEY="$AWS_SECRET_ACCESS_KEY" \
    -e RUSTFS_CONSOLE_ENABLE=false \
    "$image" /data >/dev/null
port=$(docker port "$name" 9000/tcp | head -1)
export IPPAI_TEST_S3_ENDPOINT="http://$port"
ready=false
for _ in {1..60}; do
    if curl --fail --silent --max-time 2 "$IPPAI_TEST_S3_ENDPOINT/health/ready" >/dev/null; then
        ready=true
        break
    fi
    sleep 1
done
if [ "$ready" != true ]; then
    echo 'RustFS did not become ready' >&2
    exit 1
fi
cargo test -p ippai --bin ippai storage::rustfs_tests::rustfs_encrypted_storage -- --ignored --exact --nocapture
