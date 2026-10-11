# Documenso, pinned. The container runs this unchanged image; everything
# instance-specific (URL, database, signing certificate) arrives as
# environment from the Worker (see src/index.ts). Building it here, rather
# than pointing wrangler at Docker Hub, puts a copy in Cloudflare's
# registry: Cloudflare doesn't cache Docker Hub pulls, so every cold start
# would re-download the image and risk Docker Hub's rate limits.
# v2.19.0 (tested end to end locally with scripts/onboarding-e2e.sh, which
# runs this exact image). Dependabot proposes new versions (tag and digest).
FROM docker.io/documenso/documenso:v2.19.0@sha256:a0592ef8edbc8d74e5e236d9c2dffb3fc4f0104a3ea1c52baaae4673799d8d91
