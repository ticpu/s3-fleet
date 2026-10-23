Perform a release of s3-fleet: the crate on crates.io, and the static binaries and .deb packages on GitHub and apt.ticpu.net.

Optional override: $ARGUMENTS (format: vX.Y.Z). If provided, use that version.

## Version determination

1. Find the last release tag (`git tag --sort=-v:refname | head -1`).
2. Examine commits since that tag to classify the release type. The crate is a library as well as a CLI, so its public API counts:
   - **Patch**: only bug fixes, dependency bumps, build changes, docs.
   - **Minor**: new features (`feat:`), user-visible behaviour changes, additions to the public API. While the version is 0.x, a breaking API change is also minor.
   - **Major** (from 1.0 on): breaking API, CLI, exit-code, `--json` or fleet.yaml changes.
3. Bump the version accordingly. If **major**, or if fleet.yaml's `version` or the TXT record's `v=` would change, stop and confirm before proceeding.

## Pre-release checks

Run in sequence; stop and report on any failure:

```sh
cargo fmt --all
cargo clippy --fix --allow-dirty --message-format=short
set -a; . scratch/s3-test.env; set +a   # live S3 tests; CI skips them
cargo test --release -- --quiet
cargo publish --dry-run
make deb
```

Without `scratch/s3-test.env` the live tests skip themselves; say so in the report rather than claiming they ran.

## Steps

1. Bump `version` in `Cargo.toml`.

2. Run the pre-release checks above.

3. Draft a changelog from `git log --oneline <last-tag>..HEAD`.

   **Rules:**
   - Group under: `New features:`, `Bug fixes:`, `Library:`, `Build:` (omit empty sections).
   - Describe user-visible behaviour, not implementation details. API additions and changes go under `Library:`.
   - Merge related commits for the same feature into one bullet.
   - No git hashes, no raw commit subjects, no co-author lines.

   Tag annotation format:
   ```
   vX.Y.Z

   New features:
   - what changed

   Bug fixes:
   - what was fixed

   Library:
   - what changed in the API

   Build:
   - what changed
   ```

4. Commit the release, with Cargo.lock force-added (it is ignored outside release commits), and push:

```sh
git add Cargo.toml && git add -f Cargo.lock
git commit -F - <<'EOF'
release: vX.Y.Z
EOF
git push
```

5. Wait for CI to be green before tagging: `./watch-ci.sh`. It pins the workflow and the full commit SHA and passes `--exit-status`. Never select a run with `--branch ... --limit 1`, which reads whatever ran last on the branch, and never tag on red or pending CI.

6. Tag (signed, annotated, the changelog as message) and push the tag; GitHub Actions builds the release:

```sh
git tag -as vX.Y.Z -m "$(cat <<'EOF'
vX.Y.Z

<changelog>
EOF
)"
git push origin vX.Y.Z
```

7. WAIT for the tag's run: `./watch-ci.sh vX.Y.Z`. It leaves a **draft** release carrying `s3-fleet_X.Y.Z_linux_{amd64,arm64}`, `s3-fleet_X.Y.Z_{amd64,arm64}.deb` and `SHA256SUMS`.

8. Sign the assets and publish the release:

```sh
./sign-release.sh vX.Y.Z
```

   Signs every asset with the key from `git config user.signingkey`, uploads the `.asc` files, then makes the release public as its last act. Assets cannot be added once a release is published, so signing never happens after. `-n` is a dry run that signs and verifies without uploading or publishing.

9. Publish the crate:

```sh
cargo publish
```

10. Publish the Debian packages to apt.ticpu.net, from `~/GIT/apt-ticpu-net`:

```sh
cd ~/GIT/apt-ticpu-net && ./ingest.sh s3-fleet vX.Y.Z
```

    It downloads the release's `.deb` assets, verifies their signatures against the archive key, checks each package's version against the tag, files them into every suite `projects.yaml` lists for s3-fleet, and publishes. `-n` inspects without touching the archive.

11. Report the tag, the crates.io version and the changelog.

## Important

- The tag is IMMUTABLE once pushed, and a crates.io version can only be yanked, never replaced. Wrong? Make a new patch release.
- Cargo.lock is committed only on the release commit.
- ARM64 smoke test if needed: `qemu-aarch64 dist/s3-fleet.arm64 --version`.
