# s3-fleet

A fleet of SSH hosts kept as one YAML document in an S3 bucket: each host's pinned host keys and labels, plus fleet-wide attributes. A Rust library, and a command-line tool for scripts and people.

- Readers need only the fleet name. The S3 endpoint is found through a DNS TXT record, and the document is world-readable, so listing hosts or exporting their keys needs no credentials.
- Host keys are pinned once, with `ssh-keyscan`, and a changed key is refused until someone says `--rekey`.
- Every write is conditional (`If-Match` on the ETag that was read): two people editing at once never lose each other's change. The second one gets exit code 4 and reruns.
- `s3-fleet ssh` and the `KnownHostsCommand` mode make ssh trust exactly the pinned keys and nothing else.

## Quick start

```sh
# once per site: publish the endpoint (see "Discovery"), then
s3-fleet --fleet prod-sbc setup        # asks for the S3 write keys
s3-fleet --fleet prod-sbc init         # creates the bucket and an empty fleet
s3-fleet --fleet prod-sbc add sbc1.example.net sbc2.example.net -l env=prod -l role=sbc

# anyone, no credentials
export S3_FLEET=prod-sbc
s3-fleet list -l role=sbc
s3-fleet list --json | jq -r '.hosts[].name'
s3-fleet ssh sbc1.example.net uptime
```

## Discovery

A fleet name is a bucket name, and the endpoint comes from DNS. For fleet `prod-sbc`, `_s3-fleet.prod-sbc` is looked up first, then `_s3-fleet`, both as relative names, so the resolver's search list applies:

```
_s3-fleet           IN TXT "v=s3-fleet1 endpoint=s3.example.net region=default"
_s3-fleet.prod-sbc  IN TXT "v=s3-fleet1 endpoint=s3-prod.example.net:7480 region=prod"
```

`endpoint` is `host[:port]` (brackets around an IPv6 literal), spoken over HTTPS with path-style requests; `region` is the signing region (radosgw's zonegroup api_name). `--endpoint` with `--region`, `S3_FLEET_ENDPOINT` with `S3_FLEET_REGION`, or a config file override discovery.

## Configuration

Optional. `/etc/s3-fleet.yaml`, then `$XDG_CONFIG_HOME/s3-fleet.yaml`, the later winning; `--config FILE` uses that file alone.

```yaml
endpoint: s3.example.net
region: default
credentials:
  default:
    access_key: ...
    secret_key: ...
  prod-sbc:
    access_key: ...
    secret_key: ...
```

Credentials are needed only to write. `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (and `AWS_SESSION_TOKEN`) win; otherwise `credentials.<fleet>` from any file, then `credentials.default`. A file holding credentials that its group can write or others can read is refused. `s3-fleet setup` writes the user's file with mode 0600.

The HTTP client honours the system trust store, `SSL_CERT_FILE`, and the proxy variables.

## Commands

| command | does |
|---|---|
| `setup` | ask for write keys on the terminal, check them, store them |
| `init` | create the bucket and an empty document; never overwrites |
| `list [-l SEL]... [--json]` | host names, one per line, or JSON |
| `show HOST [--json]` | one host |
| `add HOST... [-l k=v]... [--port N] [--key-type T]... [--rekey] [--keep-going]` | keyscan and pin |
| `rm HOST... [--keep-going]` | remove hosts |
| `label HOST... k=v... k-...` | set (`k=v`) or remove (`k-`) labels |
| `attr set k=v...`, `attr rm k...`, `attr get k` | fleet-wide attributes |
| `known-hosts [-l SEL]... [--host H] [--cached] [-o FILE]` | known_hosts lines |
| `ssh [USER@]HOST [ARGS...]` | ssh trusting only the pinned keys |

A selector is comma-separated terms, all of which must hold: `k=v`, `k!=v` (absent or different), `k` (set), `!k` (absent). Repeated `-l` are ANDed too.

`add` and `rm` are all or nothing: if one host fails, nothing is written. With `--keep-going` the rest is committed and the exit code is 3. Each failure is printed on its own line, `s3-fleet: <host>: <error>`.

### Contract for scripts

- stdout carries only data; diagnostics go to stderr.
- No command reads stdin or prompts, except `setup`, which refuses to run without a terminal.
- `--json` output carries `"format": 1`, and its shape changes only with that number:

```json
{"format":1,"fleet":"prod-sbc","endpoint":"s3.example.net","endpoint_source":"dns _s3-fleet.example.net.","source":"s3","age_secs":null,"attributes":{},"hosts":[{"name":"sbc1.example.net","port":22,"keys":["ssh-ed25519 AAAA..."],"labels":{"env":"prod","role":"sbc"}}]}
```

Exit codes:

| code | meaning |
|---|---|
| 0 | success |
| 1 | any other error |
| 2 | usage error, or `setup` without a terminal |
| 3 | partial: `--keep-going` committed some hosts, others failed |
| 4 | conflict: someone else wrote first, nothing was written; rerun |
| 5 | the fleet does not exist, or is not public and the request was anonymous |
| 6 | credentials missing or refused |
| 7 | a host's key differs from the pinned one; `add --rekey` accepts it |
| 8 | S3 unreachable, and no usable cache (or no `--allow-stale`) |

### Offline and stale reads

Every successful read refreshes a copy in `$XDG_CACHE_HOME/s3-fleet/<fleet>.yaml`. With `--allow-stale`, a read that cannot reach S3 uses that copy, warns with its age, and reports `"source": "cache"` and `age_secs` in JSON. Without it, an unreachable S3 is exit 8.

## ssh

`s3-fleet ssh` refreshes the cache, then execs ssh with:

```
-o KnownHostsCommand='s3-fleet --fleet F known-hosts --cached --host %H'
-o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null
-o StrictHostKeyChecking=yes -o CheckHostIP=no
```

A script can pass the same options to plain ssh. `known-hosts --cached` never touches the network, since ssh may call it up to three times per connection. Alternatively, `s3-fleet known-hosts -o FILE` writes a file for `-o UserKnownHostsFile=FILE -o StrictHostKeyChecking=yes`.

## Shell completion

```sh
source <(COMPLETE=bash s3-fleet)    # also zsh, fish, elvish, powershell
```

Host names are completed from the local cache of `$S3_FLEET`, or of every cached fleet.

## The fleet document

`fleet.yaml` at the bucket root, written world-readable (`x-amz-acl: public-read`):

```yaml
version: 1
attributes:
  ssh_timeout_secs: '3'
hosts:
- name: sbc1.example.net
  keys:
  - ssh-ed25519 AAAA...
  labels:
    env: prod
- name: sbc9.example.net
  port: 2222
  keys:
  - ssh-ed25519 AAAA...
```

Hosts are sorted by name and maps by key, so the bytes are stable. Unknown fields and versions are refused rather than dropped, so an older s3-fleet cannot erase what a newer one wrote.

## Library

```rust
use s3_fleet::{Fleet, discover, s3::S3};

let found = discover::discover("prod-sbc").await?;
let fleet = Fleet::new("prod-sbc", S3::new(found.endpoint, credentials, timeout)?)?;
let snapshot = fleet.load().await?;
let mut doc = snapshot.doc.clone();
doc.set_label("sbc1.example.net", "role", "sbc")?;
fleet.commit(snapshot, &doc).await?; // Error::Conflict if someone wrote first
```

Build with `default-features = false` to leave out the command-line dependencies. The library never retries, never falls back to the cache on its own, and never prompts.

## Release binaries

Static musl binaries for x86_64 and aarch64 are attached to each GitHub release, with `SHA256SUMS` and detached signatures. `./build.sh` builds both locally in one podman or docker pass.

## License

MIT OR Apache-2.0.
