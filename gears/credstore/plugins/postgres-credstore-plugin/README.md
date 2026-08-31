# postgres-credstore-plugin

A `CredStore` backend plugin that **persists secret values in a relational
database**, so a secret written through the credstore API survives the process
— or the container — being recreated.

The alternative in this workspace, `static-credstore-plugin`, keeps every value
in a `HashMap` in memory. That is correct for a development seed store and fatal
for anything a human typed in: every stored secret dies with the process. This
plugin exists to end that.

It implements the same three-method plugin SPI, `CredStorePluginClientV1`
(`get`/`put`/`delete`). It changes nothing about the credstore **gear** or the
SPI, and `static-credstore-plugin` stays compiled in and usable.

## Values are stored unencrypted

`credstore_plugin_values.secret_value` is `BYTEA` (`BLOB` on `SQLite`) holding
the secret's raw bytes, **in plaintext**.

This is a ratified decision, not an oversight. The benchmark system this
replaces stores SSH private keys as plaintext `TEXT`
(`vhp-testrunner: ssh_keys.private_key`) with no encryption crate anywhere in
its dependency tree, and this codebase has no at-rest encryption, KMS
integration or key-derivation pattern to build on — the only crypto in credstore
is an HMAC integrity *fingerprint* whose own key lives in the plaintext value
backend. Adding encryption here would invent that precedent in a plugin.

The consequence, on the record: **a `pg_dump` of the credstore database is a
file of private keys**, exactly as a dump of the legacy database already is.

## Do not enable `TRACE` logging for this gear

`DEBUG` is safe: a break-tested leak test
(`src/infra/storage/leak_tests.rs`) captures every event in the process — the
plugin's own logs, `sqlx`'s statement logging, and the storage-failure path —
and proves no secret byte appears, in raw, hex, or decimal-array form.

`TRACE` is not. `sea-orm` annotates every driver entry point with
`#[instrument(level = "trace")]`, and the span it opens carries the whole
`Statement` including its bound `values`, so a secret's bytes are rendered as a
decimal array into any event emitted inside it. The plugin cannot suppress that
— the value must be bound as a parameter, and `sea-orm` offers no hook to redact
a span field. `tests/sea_orm_trace_exposure.rs` pins the behaviour so a
`sea-orm` upgrade that changes it is noticed.

## What is persisted, and what is not

The SPI has two runtime-written key classes, selected by the `owner_id`
argument, and the plugin config can seed two more read-only ones.

| key class | selected by | storage |
|---|---|---|
| `private` | `owner_id = Some` | table row (`owner_id NOT NULL`) |
| `tenant`  | `owner_id = None` | table row (`owner_id NULL`) |
| `shared`  | config only | in memory |
| `global`  | config only | in memory |

Only `private` and `tenant` are ever written by `put`/`delete`, so only those
need to survive a restart. `shared`/`global` have no write path: they are
rebuilt from configuration on every boot, which is also what keeps the in-memory
plugin's invariant that a tenant-scoped delete can never destroy an entry
serving other tenants. The read order is therefore unchanged from the in-memory
plugin: `tenant (row) → shared → global` for `owner_id = None`, and the private
row alone for `owner_id = Some`.

### One deliberate divergence from the in-memory plugin

Config-seeded `tenant`/`private` entries are written **insert-if-absent, not
upsert** (`Service::seed`). The in-memory plugin rebuilds its maps from YAML on
every boot, so a value rotated through the API silently reverts on restart. In a
*persistent* backend that would destroy secret material a client wrote — the
exact failure this plugin exists to end — so a stored value always wins over the
configured one. A first boot behaves identically.

## Schema

One table, in whichever database the gear's `database:` stanza names.

```
credstore_plugin_values(
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    owner_id UUID NULL,              -- NULL = tenant class, NOT NULL = private class
    secret_ref TEXT NOT NULL CHECK (length BETWEEN 1 AND 255),
    secret_value BYTEA NOT NULL,     -- plaintext, see above
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
)
```

### Two partial unique indexes, not one composite index

```sql
CREATE UNIQUE INDEX uq_credstore_plugin_values_tenant
    ON credstore_plugin_values (tenant_id, secret_ref)            WHERE owner_id IS NULL;
CREATE UNIQUE INDEX uq_credstore_plugin_values_private
    ON credstore_plugin_values (tenant_id, owner_id, secret_ref)  WHERE owner_id IS NOT NULL;
```

A single `UNIQUE (tenant_id, owner_id, secret_ref)` would **not** keep the
tenant class unique: `NULL` is not equal to `NULL` in a unique index, so that
one index admits unlimited duplicate rows for the same `(tenant_id,
secret_ref)`. Verified empirically — on PostgreSQL 16 the composite index
accepts two identical tenant-class rows, while the two partial indexes reject
duplicates in both classes and still allow two owners to hold one reference.
`PostgreSQL 15+` could opt out with `UNIQUE NULLS NOT DISTINCT`; `SQLite` has no
equivalent and this migration must build the same shape on both.

## Sharing a database with the credstore gear

`dbname: credstore` alongside the credstore gear is safe and is the intended
deployment. Each gear's applied-migration list lives in its own history table
(`toolkit_migrations__postgres_credstore_plugin__87147a49` versus
`toolkit_migrations__credstore__<hash8>`), and the entity tables
(`credstore_plugin_values` versus `credstore_secrets`) do not collide.

Unlike the workspace's two other DB-owning plugins, which take their own DSN and
run `sqlx::migrate!` inside `init`, this plugin declares `capabilities = [db]`
and lets the platform's DB phase resolve the connection and run its `SeaORM`
migrations. That is what produces the namespaced history table, and it avoids a
second credential and a second pool.

## Tenant isolation

Every query runs through `SecureORM` with `AccessScope::for_tenant(tenant_id)`,
so the tenant clamp is applied by the ORM rather than by a hand-written
predicate that could be forgotten. The key class is then selected explicitly
(`owner_id = o` or `owner_id IS NULL`). The in-memory plugin's tenant/owner
scoping is preserved in full; the legacy system has none, and matching it there
would be a downgrade.

## Selecting this plugin

Two independent switches, both off by default:

1. **Compile it in.** `cargo build --features postgres-credstore`, or add
   `postgres-credstore` to the image's `CARGO_FEATURES`.
2. **Point the gear at it.** Set `credstore.config.vendor` to
   `constructorfabric-postgres` (this plugin's default vendor; the in-memory
   plugin's is `constructorfabric`). Selection is an exact vendor-string match,
   so the two plugins coexist in one binary with no priority tie-break.

Either change alone is a no-op. Reverting is the same two lines in reverse.

The gear also needs a `database:` stanza; it fails closed at `init` without one
rather than coming up as a silent value-losing store.

## Tests

```bash
cargo test -p cf-gears-postgres-credstore-plugin
```

Everything runs against a real `SeaORM`/`SecureORM` path over `SQLite`, with the
schema built from the migration definitions. Restart survival uses a
**file-backed** database, because a `mode=memory` `SQLite` database is destroyed
when its last connection closes — dropping the handle would destroy the evidence.

The real-PostgreSQL suite is skipped unless a DSN is provided:

```bash
docker run -d --name scratch-pg -e POSTGRES_PASSWORD=scratch \
    -e POSTGRES_USER=scratch -e POSTGRES_DB=credstore \
    -p 127.0.0.1:55433:5432 postgres:16-alpine
CREDSTORE_PG_TEST_DSN=postgres://scratch:scratch@127.0.0.1:55433/credstore \
    cargo test -p cf-gears-postgres-credstore-plugin --test restart_survival_pg
```
