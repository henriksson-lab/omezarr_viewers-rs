# Quality plan

Written 2026-08-30, after the annotation work (PLAN.md phases 8-11) roughly
doubled the frontend and added three new formats. `PLAN.md` tracks *features*;
this tracks the work needed to keep them trustworthy. Tick items off in place
(`- [x]`) as they land, as `PLAN.md` does.

The ordering is deliberate: **tasks 1 and 2 protect what already works** and are
cheap; **3 and 5 make the next change safe**; **4 and 6 raise the floor**. Doing
6 before 1 would be polishing a house with no locks on it.

Each task states an acceptance criterion, because "improve quality" is not a
condition anything can be checked against.

---

## Where this came from

Measured on 2026-08-30, not estimated:

| | |
|---|---|
| Rust tests | 115 (78 server unit, 35 server integration, 16 shared crate) |
| Frontend unit tests | **0** |
| Browser assertions | 107 — **none of them in the repo** |
| `cargo test` in CI | `-p server` only, so the shared crate's 16 tests never run |
| Largest file | `app/src/app.rs`, 3406 lines, with a 1221-line `update()` over 96 message variants |
| Second largest | `app/src/viewer_canvas.rs`, 1452 |
| Largest server file | `server/src/annotations/roi_table.rs`, 1403 |
| Panics outside tests | 28, of which 8 abort the whole server on bad CLI input |

## Where it ended up

Same measurements, taken 2026-08-30 after the six tasks:

| | |
|---|---|
| Rust tests | 280 (81 server unit, 159 server integration, 16 shared crate, 24 frontend) |
| Frontend unit tests | 24, on the host, no `wasm-bindgen-test` |
| Browser assertions | 76, in `tests/browser/`, run by `make test-browser` and by CI |
| `cargo test` in CI | `--workspace` |
| Largest file in `app/src/` | `webgl/renderer.rs`, 757 — `app.rs` is gone, split into eleven modules |
| Longest function | under 150 lines, everywhere in `app/src/` |
| Largest server file | `api.rs`, 1275 — untested when this was written, 124 tests now |
| Panics outside tests | 5: three `unreachable!` and two `as_object()` on a literal |

`geojson.rs` at 990 was never in this plan and is not covered by any of its
acceptance criteria; it is the obvious next file if this exercise is repeated.

---

## 7. The HTTP surface, tested — added after the six above

`api.rs` was 1275 lines, 26 routes and 47 explicit non-OK status decisions with
**no Rust test executing a single line of it**. The files in `server/tests/`
looked like integration tests but called the library directly; the browser
suites drove real routes, but through the UI, asserting on what the screen shows
rather than on status codes, headers, or anything the UI never calls.

- [x] `api::configure()` — one route list, shared by `main` and the tests.
      Ordering is behaviour (`/tables` and `/layers` are literals that also match
      `/{layer}`), so a harness with its own list would test a server nobody runs.
- [x] `server/tests/api_harness/` — a real `AppState` over the synthetic
      fixture, driven over HTTP, with fixtures for image / labels / objects /
      annotations / remote-writes-enabled.
- [x] 124 tests across seven files, covering every route and both verdicts on
      each: `api_session` 24, `api_pixels` 36, `api_objects` 21, `api_tables` 19,
      `api_annotations` 17, `api_limits` 4, `api_smoke` 3.
- [x] **The `--allow-remote-writes` gate is pinned**, and the tests were
      mutation-checked: stubbing the `if` to `false && …` fails exactly two of
      them, so they hold the gate rather than something adjacent.
- [x] **Four client-triggerable overflow panics fixed** — `api.rs` ×3 (`z +
      depth`, `index + 1`, `offset + limit`) and `zarr_reader.rs::z_range`, which
      the regression test found *after* the first three were fixed.

### Still open, found by these tests

Each is real and none is fixed; each is a judgement call about what the API
should promise rather than a defect with one obvious answer.

- [x] **An out-of-range `level` is a 500 from `/api/tile` and `/api/value` but a
      400 from `/api/slice`.** Fixed by task 8: all three validate the level
      before reading, and `a_level_outside_the_dataset_is_a_400_from_every_pixel_route`
      sweeps all three.
- [ ] **An out-of-range channel returns 200 and a black tile.** `c=9` on a
      2-channel image comes back as fill-value pixels, indistinguishable from
      data that is genuinely black, because zarrs pads the out-of-bounds subset.
      An overhanging y/x tile has a reason to pad; a nonexistent channel has none.
- [ ] **An unknown `columns=` name is silently dropped** (`api.rs`, the
      `filter_map` over requested column names). The client indexes the returned
      planes positionally, so a typo or a server-side rename shifts every later
      column's meaning instead of erroring — the numbers arrive under the wrong
      labels.
- [ ] **`POST /api/project` clears the session before opening the new one**, and
      answers 200 with an empty layer list when every layer fails. The user is
      left with nothing and the client gets no signal. `POST /api/open`
      validates first; `open_project` does not.
- [ ] **`/api/tables/{layer}/column` returns the same 400 and the same message**
      for a column that does not exist and one that is text, so a client cannot
      tell a typo from a type mismatch.
- [x] **Naming a pixel-less layer is 400 from `/tile` and `/slice` but 404 from
      `/value`**, and a 404 from `/objects` does not distinguish "no such layer"
      from "that layer has no objects". Fixed by task 8: unknown id is 404 and
      names it, wrong kind is 400 and names what the layer lacks, on every
      route.

The theme is one thing: the status-code decisions in `api.rs` were never
uniform, and until now nothing was in a position to notice.

---

## 1. Put the browser tests in the repo — **the important one**

The frontend has no unit tests at all, so 107 browser assertions are the *only*
thing verifying every drawing tool, every edit gesture, the label painting and
the table view. They currently exist as five ad-hoc Python scripts in a
scratchpad directory that is deleted when the session ends. If that happens the
entire UI becomes untested in one step, silently.

They are also brittle in a way worth fixing while moving them: coordinates are
hardcoded against one window size and one demo dataset, and selectors were
addressing controls by counting `<select>` elements — which is how renaming one
placeholder broke five assertions in a single edit.

- [x] `tests/browser/` in the repo: `cdp.py` (the CDP driver), one module per
      suite, and a `conftest`-style shared fixture that starts the server
      against `make demo`'s output on a free port and tears it down.
- [x] **A viewport-independent coordinate helper.** Every suite currently
      recomputes `world * 2.3828 + 90` by hand. One `world_to_screen()` derived
      from the canvas rect at run time, so a different window size does not
      silently move every assertion.
- [x] **Address controls by role, not by index.** The `.annot-filter` /
      `.annot-new-type` / `.annot-selected-type` hooks added under duress are
      the right pattern; finish the job for the rest of the panel.
- [x] `make test-browser`, and document the prerequisites (headless Chrome,
      `--use-angle=swiftshader --enable-unsafe-swiftshader`, `websocket-client`,
      Pillow).
- [x] **Use a private CDP port derived from the PID.** Sessions on a shared
      machine otherwise attach to *each other's* browsers — which happened here,
      and produced a window size nobody had asked for.
- [x] A CI job, `continue-on-error` at first so a flake does not block a merge,
      promoted once it has proved stable. **Promoted** — it is a required gate
      now, with a `timeout-minutes` because a browser test's failure mode is a
      hang rather than a failure. Checked by mutation: breaking one claim in the
      drawing suite makes the run exit 1.

**Acceptance:** `make test-browser` passes from a clean checkout with no
scratchpad, and deleting a rendering path fails it.
**Done** — six suites, 76 assertions, all passing. They found two real bugs on
their first run: annotation sets the server listed were never rendered in the
reopen panel, and that list never refreshed after a save.

## 2. Test the whole workspace — **one word, real coverage**

`make test` and CI both run `cargo test -p server`. The shared crate holds the
geometry: point-in-polygon with holes, hit ordering, the spatial hierarchy rule,
vertex insert/delete/move, GeoJSON serialisation. **Sixteen tests that CI has
never run.**

- [x] `cargo test --workspace` in the `Makefile` and in `ci.yml`.
- [x] Check nothing was passing only because it was never run.
- [x] Lint the workspace too, rather than naming three crates individually — a
      fourth crate would otherwise be added unlinted.

**Acceptance:** CI reports 115+ tests, not 113.
**Done** — 131. Nothing was passing only because it was never run.

## 3. Split `app.rs`

3406 lines, one `update()` of 1221 lines over 96 message variants. It is
navigable only by grep, and during phase 11 two bulk edits landed in the wrong
match arm because two arms read identically out of context. Both were caught,
but that is the failure mode this shape invites rather than an accident.

The variants already group cleanly — the names say so: 21 annotation, 9 object,
5 channel, 4 label, 4 table, 2 tile.

- [x] `app/src/app/mod.rs` keeps `App`, its state, `view` and the top-level
      dispatch.
- [x] `app/src/app/annotations.rs` — the 21 annotation messages, `apply_edit`,
      `geometry_of`, `editable`, `rebuild_annotations`.
- [x] `app/src/app/tables.rs` — table paging and the label-colouring join.
- [x] `app/src/app/tiles.rs` — tile loading, level choice, `load_visible_tiles`
      (185 lines on its own).
- [x] `app/src/app/layers_view.rs` — `view_layers`, 176 lines of markup.
- [x] Split `AppMsg` into per-area enums nested in one outer enum, so a handler
      and its messages sit together and an arm cannot be pasted into the wrong
      match.

**Acceptance:** no file in `app/src/` over ~800 lines; no function over ~150.
Behaviour unchanged — the browser suites from task 1 are the proof, which is why
this comes after them.
**Done** — largest file 757 (`webgl/renderer.rs`), longest function under 150,
and the 76 browser assertions pass unchanged at every step. `AppMsg` is now
eleven per-area enums behind one outer enum, each with its handler in its own
module. Splitting `app.rs` exposed the same shape in `viewer_canvas.rs` (1452
lines, a 414-line `update`), `layers.rs` (1194) and `api_client.rs` (857), which
the acceptance covers too; all three are module directories now.

## 4. Frontend unit tests

Plenty of `app.rs` is pure and needs no DOM, so it needs no `wasm-bindgen-test`
either — a plain `#[cfg(test)] mod tests` compiles and runs on the host.

- [x] `geometry_of` — every tool maps to the geometry it claims, a degenerate
      drag is refused, a backwards drag comes out the right way round.
- [x] `is_axis_aligned_rect` — the inspection that decides corner handles versus
      vertex handles, including the near-miss cases.
- [x] `AnnotUiState::batches` — one batch per colour, points and outlines and
      fills separated, a filtered class contributing nothing.
- [x] `class_color` — stable across orderings, which is the whole reason it is a
      hash rather than a palette.
- [x] `triangulate` — a ring, a ring with a hole, a degenerate ring, an open
      ring: fills are the one place a wrong answer is silent rather than loud.
- [x] `LayerState::measurement_lut` — the ramp, the id ceiling, non-finite
      values.

**Acceptance:** `cargo test -p app` runs on the host and covers each of the
above.
**Done** — 24 tests, no `wasm-bindgen-test` and no DOM.

## 5. Split `roi_table.rs`

1403 lines now doing five jobs: ROI-table geometry, four storage backends,
AnnData decoding, generic table reading, and both local and remote I/O. Its own
section comments already name the seams.

- [x] `annotations/table/mod.rs` — `Columns`, `RoiTable`, `Region`, the scale,
      and what makes a table geometry.
- [x] `annotations/table/backends.rs` — CSV, JSON and Parquet: bytes in, columns
      out, and nothing else.
- [x] `annotations/table/anndata.rs` — `X`/`var`/`obs`/`obsm`, the dtype
      widening, the categorical decode, and both fetchers.
- [x] `annotations/table/store.rs` — group metadata, targets, the local and
      remote paths.
- [x] ~~While moving: the sync and async AnnData fetchers still duplicate ~40
      lines of "open an array, widen it to f64".~~ **The premise was wrong.**
      There is only one AnnData fetcher. `read_async` bails on the AnnData
      backend — remote AnnData is not read at all — so there is nothing to
      deduplicate. The async array reads that exist are in `zarr_reader.rs`, for
      image tiles. Reading remote AnnData is a feature gap, not a quality one;
      `README.md` is where it belongs.

**Acceptance:** no file over ~500 lines; the backend tests move with their
backend.
**Done** — `mod` 400, `store` 344, `anndata` 276, `columns` 271, `backends` 166.
The directory is `annotations/roi_table/` rather than `annotations/table/` as
the plan wrote it: `roi_table::` is how every call site already spells it, and
the module's tests were already a file of their own there. The tests stayed in
one `tests.rs` — they are round trips through the writer, so they cross every
backend rather than belonging to one.

## 6. Catch errors instead of panicking

28 panic sites outside tests, and they are not one problem but four. Only the
first is a defect.

- [x] **`server/src/main.rs` — 8 sites, and the real one.** A bad `--store`
      path, an unreadable project file or a malformed ontology currently aborts
      the whole server with a Rust backtrace. Make `main` return
      `anyhow::Result`, print the error *chain* (`{e:#}`, which the rest of the
      codebase already uses), and exit non-zero. The user should learn their
      path was wrong, not read a panic.
- [x] **`zarr_reader.rs` — 4 lock sites.** `arrays.lock().unwrap()` turns one
      panicking thread into a cascade. The map is a *cache*; a poisoned one is
      still perfectly usable, so `unwrap_or_else(|e| e.into_inner())` is both
      more robust and more honest about what the lock protects.
- [x] **`objects/npy.rs` — 5 `try_into().unwrap()`.** Infallible, but proved by
      a length check several lines up rather than by the types. `as_chunks` or a
      helper taking a fixed-size array makes the compiler carry the proof.
- [x] **`app/src/api_client.rs` — 3.** `web_sys::window().expect("no window")`
      on every single request. Always true in a browser; compute the host once
      and keep it, so the assumption is stated once rather than thirty times.
- [x] **Leave the `unreachable!` alone.** There are three, not two
      (`source.rs`, `npy_volume.rs`, `app/src/app/labels.rs`); each is guarded
      by a match or a predicate on the same value immediately above and carries
      a message saying so. That is what `unreachable!` is for, and replacing
      them with error paths that cannot be reached would be worse.

**Acceptance:** no panic reachable from a CLI argument or a malformed input
file; `server --store /does/not/exist` prints one clear line and exits 1.
**Done** — it does:

```
omezarr-viewer: opening the store /does/not/exist: opening file:///does/not/exist: …
```

and exits 1, as does a bad `--project`, `--layer` or `--ontology`. Two sites the
plan did not list turned up and were fixed with it: `web_sys::window().unwrap()`
in the canvas's resize listener, and the `HOST` memo that replaced the three
`expect`s in `api_client`. What is left outside tests is the three
`unreachable!` and two `as_object().unwrap()` calls on a `json!` object literal
three lines above them — reachable from neither an argument nor a file.

---

## Not in this plan, and why

- **Performance.** A feature table with 100k rows, or an annotation layer with
  10k shapes, rebuilds every GPU buffer on every edit. Fine at hand-drawn scale
  and unmeasured beyond it — but *unmeasured* is the point: measure before
  optimising, and there is no benchmark yet to measure against. Worth a task of
  its own once there is a real dataset that hurts.
- **Fuzzing the parsers.** The GeoJSON and AnnData readers take untrusted files
  and should turn any input into a clean error. Only the malformed cases someone
  thought of are tested. `cargo-fuzz` over `geojson::parse` and
  `columns_from_payload` is the obvious next step after task 5, which is what
  gives them a stable home.
- **Feature gaps.** Tracked in `README.md` under "Known gaps"; this file is
  about the quality of what exists, not what is missing.

---

## 8. The duplication, measured and removed

A normalised clone detector over `server/src`, `app/src` and `src` found **71
repeated blocks of >=8 lines**. Five clusters were real; the rest were field
lists and match arms. **42 remain**, all in the noise category.

- [x] **`api.rs`'s layer-resolution preamble, 17 call sites across 15
      handlers.** The copies had drifted, which is *why* the status codes were
      inconsistent — so this was one defect, not two. One `resolve_store` plus
      its object/table/annotation siblings, and a taxonomy applied everywhere:
      unknown id 404 naming it, wrong kind 400 naming what it lacks, caller
      value out of range 400. Two sweeps in `api_session.rs` walk all eight
      layer-naming route shapes so the next drift fails a test.
- [x] **Two `.npy` header parsers.** `value_of` was byte-identical in
      `npy_volume.rs` and `objects/npy.rs`; the magic/length/dict-text handling
      was the same job written twice. Now `server/src/npy_header.rs`, which also
      houses `classify` — deciding "volume or table" is a question about the
      header. `npy_volume.rs` 697 -> 568, `objects/npy.rs` 418 -> 362. It
      surfaced a latent bug: the pre-magic guard was `< 12` in one reader and
      `< 10` in the other, and neither is right (v1 needs 10, v2 needs 12).
- [x] **The `.npy` test writer, 4x -> 1x** (`convert.rs` twice, plus both
      readers). The copies disagreed about whether `descr` arrived quoted;
      reconciled toward the form that can also express a structured dtype.
- [x] **`app/src/api_client` request boilerplate, 22 call sites.** 909 -> 832
      lines including ~150 lines of new shared code, so the call sites
      themselves roughly halved. Every error label kept its specificity.
- [x] **Three WebGL buffer uploads** onto one `upload_vertex_buffer`, plus a
      uniform-setting pair the detector found alongside them. The `unsafe`
      `Float32Array::view` now carries the `SAFETY:` comment it never had: the
      view borrows wasm linear memory, and any allocation can move it.

**Acceptance:** the browser suites pass unchanged — they assert on pixels, so
they are what proves the renderer refactor draws the same thing. 288 Rust tests,
76 browser assertions, clippy and fmt clean.

---

## 9. The second round: shared contract types, and `api.rs` split

Re-measuring after task 8 found four clusters left that were not noise, plus the
file the ≤800-line rule had never been applied to.

- [x] **`api.rs` split.** 1436 lines -> `server/src/api/` in six modules,
      largest 439. `configure()` is byte-identical (verified by `diff` against
      HEAD) because its route *order* is behaviour. Two modules are named
      `object_routes.rs` / `annotation_routes.rs` rather than `objects` /
      `annotations`: an actix route macro expands to a struct named after the
      handler, and a module would shadow it in the type namespace — which
      `desktop/src/main.rs` would have discovered, since it registers services
      by hand.
- [x] **`TileCoords` in the shared crate.** The same eight numbers were declared
      four times, and the projection beside them three different ways —
      `(kind, depth)` outbound, `(kind, z0, z1)` in the cache key, a bare
      `depth` in the query. Converting between those is where one of task 7's
      four overflow panics lived; `TileCoords::z_range` now does it once, and
      saturates.
      **Not applied to `zarr_reader::TileRequest`**, deliberately: it is an
      internal type with a builder API whose doc says the builder exists so "a
      tenth positional `u64` is a bug waiting for a caller to transpose two of
      them". Embedding would have turned ~29 reader field reads into
      `.coords.` for no gain. `TileQuery` converts rather than embeds, because
      `web::Query` goes through `serde_urlencoded`, which cannot flatten — tested,
      not assumed.
- [x] **`ObjectRegion` shared.** The client's `ObjectRegion` and the server's
      `ObjectQuery` were field-for-field identical. One type now, re-exported
      server-side under its own word for it.
- [x] **`LayerHeader` component.** Four of the five panels adopted it;
      `channel_panel` was deliberately left alone, because its checkbox is a
      *sibling* of the label rather than nested in it and it has no remove
      button at all — serving it would have needed the three-flag component that
      is worse than one honest exception.
- [x] **`zarr_reader`'s local/async metadata twins.** Diffed line by line first:
      no hidden asymmetry, only the two `open` calls. Shared assembly extracted
      on the `assemble_anndata` precedent; the fetching stays written twice,
      because no generic unifies zarrs' sync and async storage traits.

**Two of these made their file bigger, and that is the honest result:**
`zarr_reader.rs` 813 -> 837 and `app/src/controls/` 1299 -> 1328. Extracting a
small clone costs a struct and a doc comment; what it buys is that adding a
field touches one place. Worth it here, and worth saying plainly rather than
quoting only the flattering number.

**Result:** duplication 71 -> 36 blocks of >=8 lines across the two rounds.
`api.rs` is no longer the largest file in the repo — `src/annotation.rs` (1017)
and `annotations/geojson.rs` (990) are, and neither has been split.
288 Rust tests, 76 browser assertions, clippy and fmt clean.

---

## 10. A float bug the browser suites caught

`dragging a vertex moves exactly that vertex` began failing on CI only. The
dragged vertex went exactly where the pointer went; the vertex *after* it moved
too, by one ULP:

    was 379.59344482421875
    now 379.5934448242187

**The cause was `serde_json`'s float parser, not the browser.** Without the
`float_roundtrip` feature its parsing is fast but not correctly rounded. The
server's dependency graph turns that feature on transitively; the frontend's did
not. So the server parsed a coordinate exactly, the client parsed the same text
a ULP away, and the next edit wrote the drift back — a slow, silent corruption
of annotation coordinates across load/save cycles, in a codebase whose whole
annotation story is "the coordinates must not move".

- [x] `float_roundtrip` enabled for `serde_json` in `app/Cargo.toml`, with a
      comment saying why, since a bare feature flag invites removal.

Three things are worth keeping from how it was found:

* **The exact-equality assertion was right.** A tolerance would have hidden a
  real defect. It is the one place in these suites that compares floats exactly,
  and it earned its place.
* **It was only visible on a newer Chrome** by accident of subpixel coordinates,
  which is why it never failed locally. The fix for *that* is `$CHROME` and the
  version print, not a change to the test.
* **Every step of the diagnosis was measurement, not inference.** `apply_edit`
  was cleared by a unit test on the exact ring; the server was cleared by a curl
  round trip; the client was convicted by a logging proxy that captured the POST
  and the PUT and showed the value changing between them. Three hypotheses were
  wrong before that (`/dev/shm`, a port collision, a missing browser), and each
  cost a CI cycle.

---

## 11. Third round: the write path, `SliderRow`, `LayerStyle`

Re-measuring after task 9 left 36 clusters, of which three were not noise. The
other 33 were checked individually this time rather than assumed: fixture
construction inside `roi_table/tests.rs`, the `fortran_order` header the npy
work deliberately left inline, and two function *signatures* sharing seven
camera parameters.

- [x] **`App::store_edit`** — the annotation write path, which was written three
      times. Four things have to happen together (mark dirty, push the inverse
      onto the undo stack, rebuild the buffers, PUT), and three-of-four is a
      silent bug: a change that cannot be undone, or one the save button never
      hears about. The third site was `Rename`, which had **discarded** the
      server's reply where the other two applied it; unifying needed no flag, so
      rename now applies it like everything else.
      Four sites were left alone — `Added`, `Removed`, `delete_all` and
      `restructure` push different `Undo` variants, call different endpoints, and
      one deliberately does not rebuild. Folding them in would have needed three
      switches to save four lines.
- [x] **`SliderRow`** — 9 of the 26 rows wearing `slider-row` adopted it. The
      other 17 wear that class because it is the panels' row layout, not because
      they are sliders: text inputs, selects, checkboxes, buttons, a dual-range
      filter, and `channel_panel`'s opacity, which names itself with `<label>`
      and reads out in `.value`. Its props are **strings, not numbers**: the
      values are `f32` in two panels and `f64` in a third, and widening an `f32`
      to reach one numeric prop changes what lands in the DOM — `0.33f32` as an
      `f64` prints `0.33000001311302185`.
- [x] **`LayerStyle { color, opacity, size, slab }`** — the four settings every
      drawable layer has, declared once instead of four times: on
      `AnnotUiState`, on `ObjectUiState`, and again on each panel's props.
      Deliberately *not* the tempting version: passing the state structs
      wholesale as props would clone the annotations vector and run an O(n)
      `PartialEq` on every render. `keep_view_of` now copies one field where it
      copied four, which is exactly the drift this prevents.

**On the cost of the third one:** it changed 57 field accesses to collapse four
declarations. That is the thinnest ratio of anything in this file, and it was
called out as such before it was done. What it bought is that both mirrors the
detector had been reporting are gone, and a new visual setting is one edit.

**Result:** duplication 36 -> 21 blocks of >=8 lines; 71 -> 21 across the three
rounds. 288 Rust tests, 76 browser assertions on **both** Chrome 104 and Chrome
152, clippy and fmt clean.

---

## 12. The last of it

Switching from a line-window detector to a **function-level** one — identical
bodies, anywhere in the workspace — found what the window detector had been
splitting across several small hits.

- [x] **`project` (31 lines) and `f32_bytes` (7) were byte-for-byte identical**
      in `zarr_reader.rs` and `npy_volume.rs`. Now `server/src/pixels.rs`. The
      readers genuinely differ — one walks zarr chunks, the other a
      memory-mapped `.npy` — but by the time they hold `&[f32]` they are doing
      the same thing, and a z-projection that drifted between them would show as
      one layer kind projecting differently from another.
- [x] **`record_target`** in `annotation_routes.rs` — the "remember where this
      layer saved to" step, beside both successful writes. Same argument as
      `store_edit`: a save path that forgot it would leave the set looking
      unsaved and send the next save somewhere else. The response JSON stays per
      branch, because the two forms genuinely report different things.

**There are now zero identical functions across files**, and 16 line-windows
left, all of which have been read: fixture construction inside
`roi_table/tests.rs`, the deliberately-inline `fortran_order` header, function
*signatures* sharing parameter lists, and handler preambles already served by
`resolve_store` and its siblings.

**This is the point to stop.** Four rounds took duplication from 71 windows to
16, and the last round's two finds were 38 lines. What remains costs more to
remove than it costs to keep — the pattern across all four rounds is that
extracting a small clone adds a struct and a doc comment, so `zarr_reader.rs`,
`app/src/controls/` and others each ended *larger*. That is the right trade for
a rule that must not drift, and the wrong one for four lines of scaffolding.

The real remaining debt is not duplication:

- [ ] `src/annotation.rs` (1017 lines) and `server/src/annotations/geojson.rs`
      (990) are the largest files and are unsplit. `api.rs` was safe to split
      because 124 tests held it; neither of these has that, and `geojson.rs`
      parses untrusted files.
- [ ] The parsers are still unfuzzed — the item task 5 deferred, whose
      precondition (a stable home for them) has been met since.

