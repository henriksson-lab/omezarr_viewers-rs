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

### Found by these tests — all now closed

Each was real, and each was a judgement call about what the API should promise
rather than a defect with one obvious answer. The two marked by task 8 were
fixed then; the four remaining were fixed by task 13.

- [x] **An out-of-range `level` is a 500 from `/api/tile` and `/api/value` but a
      400 from `/api/slice`.** Fixed by task 8: all three validate the level
      before reading, and `a_level_outside_the_dataset_is_a_400_from_every_pixel_route`
      sweeps all three.
- [x] **An out-of-range channel returns 200 and a black tile.** `c=9` on a
      2-channel image comes back as fill-value pixels, indistinguishable from
      data that is genuinely black, because zarrs pads the out-of-bounds subset.
      An overhanging y/x tile has a reason to pad; a nonexistent channel has none.
      Fixed by task 13: `check_channel` beside `check_level`, on all three pixel
      routes. A volume with **no `c` axis is exempt** — the index names nothing
      there, exactly as `t` already did — because refusing would 400 every tile
      of an ordinary `(z,y,x)` OME-Zarr.
- [x] **An unknown `columns=` name is silently dropped** (`api.rs`, the
      `filter_map` over requested column names). The client indexes the returned
      planes positionally, so a typo or a server-side rename shifts every later
      column's meaning instead of erroring — the numbers arrive under the wrong
      labels. Fixed by task 13: **refused**, not reported. The ROI-table
      precedent reports because that save is still correct, merely lossy; this
      one is not correct, and a header saying so only helps a client that
      thought to read it.
- [x] **`POST /api/project` clears the session before opening the new one**, and
      answers 200 with an empty layer list when every layer fails. The user is
      left with nothing and the client gets no signal. `POST /api/open`
      validates first; `open_project` does not. Fixed by task 13: open
      alongside, then drop. A *partial* open is still a success with
      `X-Skipped`; opening nothing out of something asked for is a 400 and the
      old session is left standing.
- [x] **`/api/tables/{layer}/column` returns the same 400 and the same message**
      for a column that does not exist and one that is text, so a client cannot
      tell a typo from a type mismatch. Fixed by task 13: 404 names it and lists
      the columns there are, 400 says what it lacks — the section-7 taxonomy one
      level down. 404 means "your column list is stale", 400 means "that column
      can never colour anything".
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

- [x] `server/src/annotations/geojson.rs` (1116 by the time it was done — it had
      grown to 1116 from 990 while the supervision work went in) is split; see
      task 14. `src/annotation.rs` (1049) is still unsplit.
- [x] The parsers are still unfuzzed — the item task 5 deferred, whose
      precondition (a stable home for them) has been met since. Done in task 15,
      and it found a panic on its first run.


---

## 13. Supervision, the plane cache, and a bug only two repositories together could show

Four strands, three of them in parallel.

### The API's last four open findings

All four from section 7 are closed, above. The pattern in the fixes is one
choice made four times: **an ambiguous success is worse than a refusal**, except
where the operation really did succeed. So a nonexistent channel is a 400, an
unknown column name is a 400, a project that opens *some* layers is a 200 with
`X-Skipped`, and a typo is told apart from a type mismatch by status code rather
than by prose.

### Stroke width and dense region have controls, and are drawn

The two fields added for partial supervision were storage-only: reachable over
the API, invisible in the viewer. Both now have controls, both are drawn, and
the layer's supervision state is stated in words — *"No dense region: every
pixel nothing covers is unexamined."*

The drawing is the part that needed care. A stroke is a claim about **pixels**,
so the band is real geometry in world coordinates rather than a wider line:
`lineWidth` above 1 is not portable in WebGL2 and is screen-space where it works
at all, and a scribble whose apparent width changed with the zoom would be
showing an assertion nobody made. A dense region is hatched rather than filled,
because it means something an ordinary region does not and so must not look like
one.

- [x] **The draft shape was a bare centreline** — the band appeared on mouse-up,
      so the shape you were about to get was not the shape you could see. The
      canvas now takes a `draft_stroke_width` prop, resolved by the app from the
      same two inputs `finish_drawing` uses. The tool→open-path shortcut it
      needs (there is no geometry yet to ask about) is pinned to `geometry_of`
      by `a_tool_draws_an_open_path_exactly_when_its_geometry_is_one`, over
      every `Tool` variant, so the two cannot drift.
- [x] **Hit-testing grabbed a scribble by its centreline.** `near` is a fixed
      number of *screen* pixels expressed in world ones, so it shrinks as the
      view zooms in: a 24-px band at 4x fit is 96 screen pixels wide and only
      the middle 10 of them answered a shift-click. `grab_reach` takes the
      larger of the hand's tolerance and half the stroke width — `near` stays
      the floor, so a band narrower than a hand is steady does not become
      *harder* to hit than a bare line. The body test uses it too: the bounds
      are the vertices, and the band stands half its width outside them.

### A plane cache for the ortho panes

Each pane re-fetched its plane on every move. The panes now cache by
`(layer, axis, level, index, t, channel, transpose)` — `transpose` included
because the texture is uploaded *after* it, so a plane read for the other
orientation is a wrong picture rather than a slow one. Pane size is deliberately
not in the key: it decides the `level`, and the level is in the key.

`TileStore` became generic over its key rather than being copied, and gained one
rule: **trim never evicts the entry just inserted.** A single plane can be a
large fraction of a pane's budget where a tile never is (1 MB against 256), and
a store that instantly forgets what it was handed is worse than one briefly over
budget by one item.

### The cross-repo golden fixture — which justified itself immediately

Nothing had run the annotation pipeline end to end. The viewer writes fragments;
`blockflow` rasterises them; each was tested thoroughly against its own idea of
the format, which is exactly the drift `objects/table.rs` warns about: *"every
consumer writes its own parser against a layout documented, at best, in the
producer's header — and the layouts drift, because nothing can compare them."*

`server/tests/fragments_golden.rs` pins a canonical scene — a polygon with a
hole, an open stroke, a dense rectangle — to committed bytes, and
`blockflow/tests/viewer_fragments.rs` reads **the same file** and rasterises it.

On its first run it failed, on a real bug neither repository could have found
alone: the viewer numbered classes from **0**, and the rasteriser reserves 0 for
*no shape covers this voxel*. Both sides were correct by their own lights and
the pipeline did not work. Class ids are now one-based, with the reason on the
field rather than left as a convention.

The fixture is **copied**, not shared through a path: tying two repositories to
a directory layout neither controls is a worse coupling than a file that has to
be copied when it deliberately changes, and the test says so when it fails.

### A suite for what only pixels can answer

`tests/browser/suites/supervision.py` — 14 checks. The band is measured at 58px
against the 57px its 24 world-pixel width asks for, and grows to 92px when the
image is zoomed, which is the whole claim: the width is a size *in the image*.
The hatch is measured as a **fraction of the region's area** (4.8%), which
distinguishes it from a fill; a single probe passed or failed on whether it
happened to land between hatch lines 26px apart.

It grew to 17 checks with the two items above: the draft measures **58px
mid-drag against 58px once stored** — the shape you see is the shape you get —
and a shift-click 9 world px off a scribble's centreline lands on it while one
24 px off still misses, which is the band being a bound rather than an excuse
for an unbounded target.

Two measurement traps caught while writing it, both the same shape as earlier
rounds: a plain before/after around a *draw* also catches the previous shape
losing its selection highlight, and read 476px for a 57px band; and
`Viewer.to_screen` is fit-based and blind to zoom, so a scale computed from it
cannot detect that the camera moved. Both are avoided the way `picking` already
avoided them — toggle the layer, shoot twice at one camera position.

---

## 14. Splitting `geojson.rs`, along a seam that was already there

The measurement that decided the shape of this: **`parse` and `write` are called
by nothing outside the file.** All 19 external call sites are store-side —
`load`, `save`, `save_async`, `split_target`, `is_annotation_target`. The codec
was already an internal detail with a store facade around it, so the cut follows
the call graph rather than an idea imposed on it.

1116 lines became four files:

| | lines | what |
|---|---|---|
| `geojson/mod.rs` | 259 | the dialect documentation, the four property-name constants, the re-exports, the shared fixtures and the round-trip tests |
| `geojson/read.rs` | 355 | `parse` and the readers — **the fuzz target's home** |
| `geojson/store.rs` | 377 | `AnnotationFile`, `attributes()`, target naming, the sync, async and bare-file operations |
| `geojson/write.rs` | 197 | `write` and `write_feature` |

The total went **up**, 1116 to 1188, and that is the same trade the four
duplication rounds kept making: the 72 lines are four module headers saying what
each half is for, four import blocks, and three test scaffolds. The largest file
anyone now has to hold in their head is 377 lines rather than 1116.

Three decisions worth keeping:

**The round-trip tests went to `mod.rs`, not to either half.** They assert the
contract *between* read and write — that anything the reader understood survives
being written and read again, including members nothing here displays. Filed
under `read` they would read as read tests and could be weakened along with it.
The two fixtures (`QUPATH`, a real QuPath export, and `square`) are shared for
the same reason: two divergent copies would let one half be tested against a
file the other half never sees.

**Sync and async stayed together in `store.rs`.** That is the obvious next cut
and it is the wrong one: the two paths exist only because zarrs' sync and async
storage traits are different types no generic unifies, so they have to stay in
step, and separating them makes drift easier and invisible. Adjacency is doing
work.

**The store half kept no unit tests**, because it never had any — its behaviour
is covered end to end by the 18 async tests in `server/tests/annotations.rs`,
and what matters about a save is that a *session* can read it back.

Verified as a move rather than a rewrite: every function, constant and struct in
the original is present afterwards, and so is every test, both checked by diffing
the item lists against `git show HEAD:`. 115/115 browser checks still pass.

- [x] Superseded by task 16, which measured a worse gap than file size:
      `input.rs` had 637 lines of interaction rules and no tests. The split plan
      for `src/annotation.rs` still stands — `geometry.rs` for the `Geometry`
      algebra and its helpers (~330), `mod.rs` for `Annotation`, `Plane` and
      `ObjectType` (~260), `hierarchy.rs` for `pick_annotation` /
      `containing_parent` / `in_tree_order` (~105), the smallest-covering-shape
      rule being a different idea from the shapes — and is the next one.

---

## 15. Fuzzing the parsers, which found a panic on the first run

Three entry points take bytes this viewer did not write, from `file://`,
`http(s)://` and `s3://` alike: `geojson::parse`, `objects::table::read`, and
`npy_header::split`/`classify`. A malformed file should come back as an **error**, because
an error names the file and the number that was wrong.

**What a panic here actually costs, measured rather than assumed:** nothing sets
`panic = "abort"`, so a panic unwinds. The server stays up and the client gets a
dropped connection instead of a message, and the log says `capacity overflow`
rather than which file was bad. That is a *robustness and diagnosability*
argument, not a safety one — worth ten lines of checking, not worth dressing up
as more than it is.

### What it found

`server/tests/parser_fuzz.rs` failed on its first execution with `capacity
overflow`, out of `Vec::with_capacity` in `table::read`. Chasing it turned up
three instances of one mistake:

* `Vec::with_capacity(column_count)` — the count is a `u64` **the file chooses**.
* `row_count * width` — a row count near `usize::MAX` overflows the multiply.
  In debug that panics outright. In release it wraps, and the wrapped product
  can match the words present — but a wrap needs `row_count >= 2^62`, and that
  value then reaches `Vec::with_capacity`, whose product with the element size
  exceeds `isize::MAX`. So it is a capacity-overflow panic there too. **There is
  no input that makes this return wrong rows** — an earlier draft of this
  section claimed there was, and working it through says otherwise.
* `at + name_words` — a name length near `usize::MAX` overflows the range's end
  and indexes from zero.

One case does not panic and is worth naming: a count like `1 << 30` is
representable, so `Vec::with_capacity` succeeds and quietly reserves ~25 GB of
address space under Linux overcommit before the length check rejects the file.
Harmless on a workstation; an OOM kill under a cgroup memory limit.

All three are now checked before anything allocates or multiplies, and each has
a named regression test rather than a committed crash file: a test called
`a_column_count_larger_than_the_blob_is_refused_rather_than_allocated` says what
was wrong and `crash-8f3a…` does not. The three were verified to **fail without
the guards** — two capacity overflows and a slice index panic — because a guard
test that passes either way is worth nothing.

`geojson::parse` and the `.npy` header parser came through clean. That is a real
result rather than a null one: `serde_json` enforces its own nesting limit, so
the recursion over `childObjects` cannot be driven deep enough to overflow the
stack, which was the specific worry.

### Two halves, because they answer different questions

**`server/tests/parser_fuzz.rs`** runs on every `make test`: a fixed generator,
a fixed seed, 20,000 mutations per parser, about a second. Deterministic on
purpose — a fuzz failure nobody can reproduce is a flake, and a flake in CI gets
muted. `FUZZ_CASES` and `FUZZ_SEED` widen it on demand.

**`fuzz/`** holds libFuzzer targets over the same three entry points for the
deep search, run with `make fuzz`. It is its own workspace so CI's stable
`cargo clippy --workspace` never tries to build it.

The mutations are not random bytes. Random bytes are rejected by the first
length check and never reach the code that indexes; what finds bugs is a
*nearly* valid file — magic word intact, one count absurd — so the seeds are
real files (the golden `fragments.bftable`, a genuine QuPath export) and one
mutation deliberately sets an aligned word to `u64::MAX`, `1 << 60` and friends.

### Run it in debug as well as release

Release turns arithmetic overflow checks **off**, and two of the three bugs only
panic with them on: a 3.2M-case release sweep across eight seeds was clean while
the bugs were still present. The clean runs that count are the debug ones.
(cargo-fuzz builds with `-Cdebug-assertions`, so the libFuzzer targets have them
either way.)

Totals after the fixes: 600k deterministic debug cases across four seeds, and
**12.9M libFuzzer executions** — 5.7M table, 2.5M geojson, 4.7M npy_header — all
clean.

---

## 16. The interaction logic, which had no tests at all

Measured rather than assumed. Three files had **zero** unit tests:

| file | lines | tests | is that wrong? |
|---|---|---|---|
| `viewer_canvas/draw.rs` | 588 | 0 | **no** — 17 GL calls deep; it needs a GPU, and the browser suites are the right level |
| `controls/annot_panel.rs` | 720 | 0 | mostly `html!` view code; low value |
| `viewer_canvas/input.rs` | 637 | 0 | **yes** — 3 GL references in 637 lines; the rest is pure decisions |

`input.rs` is where **five** of CLAUDE.md's "things that are the way they are for
a reason" live, and every one of them was a bug before it was a rule: a locked
shape offers no handles; a point is grabbed by its body and never by a corner; a
vertex beats an edge beats the body; shift is the vertex modifier, so a
shift-click that misses does nothing; and a scribble is grabbed by its band. All
five were enforced by `grab`, and checked only at whatever handful of
coordinates the browser suites happened to click.

`grab` did two lookups and then decided. The decision is now `grab_at(&Editable,
x, y, shift, near) -> Option<(Handle, EditKind)>` — pure, beside the types it
decides over — and `is_worth_keeping` became a free function, having never
touched `self`. That the extraction left `Handle`, `EditKind` and
`segment_distance` unused in `input.rs` is the evidence it was a real seam and
not a line drawn through the middle of something.

Thirteen tests, one per rule, and **every one was mutation-checked**: remove the
`locked` guard and only the lock test fails; return `Body` before looking at
vertices and the three ordering tests fail; let `shift` fall through to the body
and the shift test fails; treat a point as boxlike and the point test fails. A
rule test that passes against the mutation it names is worth nothing, and this
session has already written two of those.

The two that were *not* worth testing this way are as much of the result: a
`draw.rs` test would need a GL context to assert something a screenshot already
asserts better, and `annot_panel.rs` is markup. A zero is not automatically debt.

- [ ] `src/annotation.rs` (1049) is still unsplit; the plan for it is in task 14.

---

## 17. The metadata parser, against files this repo did not write

Every image fixture in this workspace comes from `synthetic.rs` — 17 call sites —
and `synthetic.rs` is *our own writer*. So `src/lib.rs` (420 lines, **zero
tests**), which holds the OME-NGFF structs that real `.zattrs` deserialize into,
had only ever been shown the exact shape we produce. That is the same failure
mode as the class-numbering bug in task 13: each half self-consistent, nothing
comparing them to the outside.

`tests/ngff_metadata.rs` now reads five real documents from three producers —
`omero-zarr` and Bio-Formats via the IDR, and the specification's own 0.5
examples — with `tests/data/ngff/SOURCES.md` recording the URL and pinned commit
of each. Both metadata *locations* are exercised for the first time: 0.4 at the
root of `.zattrs`, 0.5 under `ome`.

### What it found, and the second one is silent

**1. A multiscale-level `coordinateTransformations` was being dropped.** The
spec's own example pairs a dataset `scale: [1, 1]` with a multiscale-level
`scale: [10, 10]` that applies to *every* dataset. `Multiscale` had no such
field, so serde discarded it: the pixel size of that image is ten, and this
viewer read one.

**Blast radius, traced rather than assumed** — an earlier draft of this section
said such a store is "drawn ten times too small", and that is wrong. Nothing
scales the drawn image by voxel size: level placement comes from array shapes.
The declared scale reaches exactly one place, `world_scale()`, and its only
consumers are the ROI table's `*_micrometer` columns. So the real cost is that a
region saved to or read from `tables/` gets the **wrong physical size**, silently
— which matters, because micrometers in a table are the one number another tool
takes at face value, but it is not a wrong picture.

- [x] **Fixed.** `world_scale` now composes the two: the multiscale's scale
      multiplies each dataset's own, per axis. A level-0 pixel of 0.5 under a
      global 10 is five micrometres, not either number alone. A factor that is
      not finite and positive reads as *says nothing about this axis* rather
      than as a reason to discard what the other transformation said — it must
      not take the good half down with it. Four tests, all four confirmed to
      fail against the old one-sided lookup, and one of them runs the
      specification's own document off disk and asserts it reads ten.

**2. A `bioformats2raw` container root is not an image.** bf2raw — the converter
most microscopy pipelines actually run — writes a root holding only
`{"bioformats2raw.layout": 3}`, with each series in a numbered subgroup. Opening
one finds no `multiscales` and fails. `info_roi.md` describes the key; no code
reads it. Pinned as today's behaviour so that supporting it is a deliberate
change with a test that flips.

- [x] **Fixed, and it mattered more than this section first said.** The
      converter next door, `img2omezarr`, writes this layout for *everything it
      produces* — `root_attributes()` emits `bioformats2raw.layout: 3`, and its
      own tests assert it. So this was not an interop nicety: **no store from
      our own pipeline could be opened** without knowing to append `/0`, and the
      layer was then called `0`.

      `read_metadata_local` and `read_metadata_async` now resolve a container
      before parsing multiscales. The layout key and the series index each have
      the same **two shapes** `multiscales` has — at the root for 0.4, nested
      under `ome` for 0.5 — and `img2omezarr` writes both, so missing either
      would have left half its output unopenable. One series opens without
      asking; several are refused **by name**, because showing one scene of a
      slide that has three is a wrong picture that looks like a right one; an
      absent index falls back to `0`, where bioformats2raw always puts the first.

      The decision is three pure functions with unit tests, and
      `server/tests/bioformats2raw.rs` builds real containers and opens them —
      four of its five tests fail with the branch disabled, and the fifth is *an
      ordinary image store is unaffected*, which must pass either way.

      Not covered: the **async** path takes the identical decision by the
      identical route, but no test opens a container over `http(s)://` or
      `s3://` — there is no remote fixture in this repo, and this did not seem
      the place to introduce one.

- [x] **Multi-series opening, and the annotation question it forced.** A
      container now expands into **one layer per series**, in `Session::add`
      rather than inside `ZarrStore` — and that placement is the whole answer to
      the annotation question. A layer's spec becomes the *series*
      (`container.zarr/0`), so `make_target` writes annotations beside the image
      they are about, with no special case anywhere in the annotation code. A
      coordinate space declared at a container root would have been a claim
      about pixels that are not there.

      Naming: several series are `container.zarr[0]`, `[1]`, `[2]`; a single one
      keeps the store's plain name, because there is nothing to disambiguate and
      `container.zarr[0]` is just noise. `Session::add` returns `Vec<String>`
      now, which is honest — a project line can open more than one layer — and
      the four call sites and the test fixtures say `only(...)` where they mean
      one.

      `ZarrStore::open_local` still refuses a multi-series container by name.
      That is the direct-open path (`roi_table`'s `group_for`, and tests), which
      cannot expand into a session; the two behaviours are consistent — the
      session opens them all, a raw store open cannot choose.

      **The gate on that probe was wrong in the first version and every unit
      test passed anyway.** I skipped the container check for anything with a
      file extension, to avoid probing `.npy` and `.csv` — but `.zarr` *is* an
      extension by that test, so the probe never ran on a single real store. It
      showed up only on running the binary against a container. `.zarr` is a
      suffix on a directory, not a file type, and the fix is pinned by three
      tests that fail against the old gate.

### What it confirmed

Three real producers parse cleanly, including the cases the synthetic fixtures
never generate: a **channel axis with no `unit`** (a channel is not a length),
`unit: "pixel"` where another writer says micrometer, and unknown members
(`_creator`, a multiscale `metadata` block, `coefficient`/`family`/`inverted` on
a channel) ignored rather than refused. A negative result, and worth having:
those were the fields most likely to be over-strict.

### On writing the assertions

Three of the five tests failed on the first run — every one because **I had
guessed at the fixtures' contents** rather than read them: the level count, which
files carry `omero`, and where the transformation sat. The parser was right and
the test was wrong each time. Worth recording, because a fixture test written
from an assumption about a file is the same mistake as a parser written from an
assumption about a format.

---

## 18. Series are alternatives, not overlays

The consequence of task 17's multi-series opening, found by measuring what it
actually drew rather than by reading the code: a container's series all arrived
**visible**, and stacked image layers composite *additively* — only the
bottom-most replaces. Two identical series rendered at **1.75x** the brightness
of one. Two different scenes would have been a meaningless blend.

Worse, and the part that settles the design: they do not share a coordinate
space. The world is the reference image's full-resolution x/y, so a container
whose series are 256² and 1024² put the second in the first's world. That is
correct for the case the world was built for — a mask over a stain — and wrong
for scenes of a slide, which are alternatives.

`LayerInfo` gained a `visible` flag, `#[serde(default)]` to `true` so an older
client or project file still reads as "show it", and the session sets it false
for every series after the first. The frontend was hardcoding `visible: true`
at seven sites; it now honours what it is told.

**Not fixed, and deliberately:** the shared world. Making it per-layer
contradicts an architectural decision everything else rests on, and hiding the
siblings makes it invisible in practice — you see one scene at a time. Recorded
as a limitation rather than rebuilt.

### Three wrong guesses about the DOM, in one suite

`tests/browser/suites/series.py` measures this in pixels, and getting there took
three corrections that are all the same mistake:

* `.channel-header input[type=checkbox]` matches each **channel's** row as well
  as the layer's, so it reported four boxes for two layers.
* Scoping to `.channel-control` did not help: that is per channel too.
* The layer's box is in neither, and is identified by its **label being the
  layer name** — which the suite now uses, with a check that exactly two were
  found so a bad selector fails loudly instead of measuring something else.

And once the right control was clicked, the shot was taken before the newly
shown layer's tiles had arrived: 72 -> 73, which reads exactly like the layer
not drawing. With a settle it is 72 -> 126, the same 1.75x as before the fix —
now as the opt-in rather than the default.

Every one of those was found by the suite failing, which is the argument for
writing it: I had already confirmed the fix with an ad-hoc script and could have
stopped there.

---

## 19. The remote read path, against real public stores — and the bug it found

Half the reader had **no positive coverage at all**. This codebase has two paths
almost everywhere — `zarrs::filesystem` synchronously for local, an
`AsyncOpendalStore` for everything else — and every fixture reads from a temp
directory. Every remote reference in the test suite was a *negative* one: that a
URL classifies as remote, that a write is refused without
`--allow-remote-writes`, that a dead host errors. Nothing had ever opened a
store over HTTP and got pixels back.

### The stores

From [OME's own catalogue](https://github.com/ome/ome-zarr-catalog) (11 IDR
datasets, all CC BY 4.0) and the
[Open SciVis set](https://github.com/InsightSoftwareConsortium/OMEZarrOpenSciVisDatasets),
picked to cover what no local fixture can:

| store | why |
|---|---|
| `idr0062A/6001240.zarr` | an ordinary 0.4 image as `omero-zarr` writes one, with real `omero` settings |
| `idr0048A/9846151.zarr` | a **`bioformats2raw` container** — the layout `img2omezarr` writes for everything |
| `backpack.ome.zarr` | **NGFF 0.5 / zarr v3**, metadata under `ome` in `zarr.json` |

`OpenOrganelle` was probed and rejected: its `.zattrs` is `{}` at the paths
tried, so it is not a plain OME-Zarr there.

### The bug

`real_pixels_come_back_from_a_public_store` failed on its first run with
`Failed to open array at /5`.

**Metadata resolution and tile reading take different routes to an array, and
only the first knew about the series.** `read_metadata_*` opened arrays at
`{prefix}/{path}`; `read_subset` opened `/{path}`. So a store opened at a
container root described its pyramid perfectly and could not fetch a single
tile. Everything still worked end to end, because `Session::add` expands a
container and hands `ZarrStore` the *series* — the broken path is the direct
open, which is what `roi_table`'s `group_for`, the tests, and any future caller
use.

`ZarrStore` now carries the prefix and every array path goes through it.

### Where the tests live, and why in two places

`server/tests/public_stores.rs` is `#[ignore]`d — `cargo test` reports it as
ignored rather than skipping silently — and runs with `make test-network`. It
reaches the public internet, so putting it in CI would import the IDR's uptime
into our build, and a red tick meaning "somebody else's server is slow" is worse
than no tick. A failure there is as likely to be the world moving as a bug.

But a test that does not run in CI cannot be the only guard for a bug, so the
same defect is pinned **locally** by
`pixels_come_back_from_a_store_opened_at_its_container_root`, which builds a
container in a temp directory and reads its deepest level. It fails against the
un-prefixed read path. That pairing is the shape to copy: the network suite
finds what local fixtures cannot, and anything it finds gets a local test.
