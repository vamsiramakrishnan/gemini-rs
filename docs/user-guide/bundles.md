# Storing and promoting specs (bundles)

A bundle is a session spec stored under a name, with a history. Each push
stores an immutable **version**, whose id is the SHA-256 of the spec's
canonical JSON. Pushing the same spec twice gives the same version.
**Labels** such as `prod` and `staging` are movable pointers to a version. A
runtime loads `booking:prod`, and promoting or rolling back means moving the
label. Nothing is rebuilt.

```text
booking            the latest version
booking@3f2a9c1d   a version, by id or a unique prefix (at least 4 digits)
booking:prod       the version a label points to
```

Only valid specs are stored. A push that fails validation is refused, so
whatever a runtime loads has already passed it.

## From the command line

```bash
adk bundle push agent.json -m "Ask for seating preference" --label staging
adk bundle list                       # bundles and their labels
adk bundle list booking               # versions, labels, messages
adk bundle label booking prod 3f2a9c  # promote (or roll back)
adk bundle get booking:prod --out agent.json
```

The store is `--store`, else `ADK_BUNDLES`, else `./bundles`:

| Store | URI |
|---|---|
| A local directory | `bundles`, `/srv/bundles`, `file:///srv/bundles` |
| Cloud Storage | `gs://my-bucket/bundles` |

Cloud Storage uses the `gcs-store` feature, and `adk` is built with it. It
authenticates the way Google's client libraries do for a service: with
`GOOGLE_ACCESS_TOKEN` if it is set, else through the metadata server on
Cloud Run, GKE or Compute Engine, else with the `gcloud` CLI. Set
`STORAGE_EMULATOR_HOST` to use an emulator.

## From code

```rust,ignore
use gemini_adk_fluent_rs::spec::{open_store, BundleRef};

let store = open_store("gs://my-bucket/bundles")?;
let version = store.push("booking", &spec, Some("Ask for seating")).await?;
store.set_label("booking", "prod", &version.version).await?;

let (version, spec) = store.get("booking", &BundleRef::Label("prod".into())).await?;
```

`BundleRef::parse("booking:prod")` splits a reference string into a name and
a reference.

## Layout and other backends

Each bundle is stored as plain objects:

```text
<name>/versions/<id>.json        the spec
<name>/versions/<id>.meta.json   id, digest, created_at, message
<name>/labels/<label>            a version id
```

The metadata object is written last, so a version appears only once it is
complete. Every write replaces a single object, so moving a label is atomic.
To store bundles elsewhere, such as S3 or a database, implement
`BundleObjects` (read, write and list by key). Wrap it in
`ObjectBundleStore` to get versions and labels.

Names and labels are lowercase letters, digits, `-`, `_` and `.`, up to 63
characters.

## Serving bundles

`adk-runtime` serves bundles by reference and picks up a moved label without
a redeploy. See [Deploying the runtime](./deploy.md).
