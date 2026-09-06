"""index — a Python host for the native C ABI, in the standard library alone.

The point of this file is not Python. It is that the SAME sixty-one symbols that serve the browser
through WASM also serve a native process through an ordinary shared library, so adopting the engine
from a high-level language costs a `ctypes` binding and no build step, no server, and no upload.
`include/index.h` is the contract; this is one reading of it, and the same reading works for Go
cgo, Java FFM, C#, Ruby or PHP.

That count is checkable rather than decorative: `grep -c '#\[no_mangle\]' crates/index-wasm/src/lib.rs`
is 61 at ABI 12, the twelve `idx_image_*` symbols included, and `idx_abi_version()` is asserted below.

The image tier is exercised here for a specific reason. An embedding crosses as a packed
little-endian `f32` buffer, never as JSON -- and this file builds that buffer with `struct.pack`
alone. No numpy, no build step, no extension module: if a language can call `dlopen` and lay out
four bytes, it can drive the fused image query, which is what "language-neutral ABI" has to mean.

Native hosts pass their own pointers. `idx_alloc` / `idx_free` exist for WASM, where a host cannot
otherwise reach into the module's linear memory, and are deliberately unused here.

Run it:
    cargo build -p index-wasm --release
    python host/python/index_ffi.py
"""

from __future__ import annotations

import ctypes
import struct
import sys
from pathlib import Path

HIT_BYTE = 12  # doc u32, score f32, typo_bucket u32 -- little-endian, packed
SEP = b"\0"  # the wire separator, matching IDX_FIELD_SEPARATOR
U32_MAX = 0xFFFFFFFF

ROOT = Path(__file__).resolve().parent.parent.parent
CANDIDATE = [
    ROOT / "target" / "release" / "index_wasm.dll",
    ROOT / "target" / "release" / "libindex_wasm.so",
    ROOT / "target" / "release" / "libindex_wasm.dylib",
]


def _load() -> ctypes.CDLL:
    for path in CANDIDATE:
        if path.exists():
            return ctypes.CDLL(str(path))
    raise SystemExit(
        "no native library found. Build it first:\n  cargo build -p index-wasm --release\n"
        "looked in:\n  " + "\n  ".join(str(p) for p in CANDIDATE)
    )


def _bind(lib: ctypes.CDLL) -> ctypes.CDLL:
    """Declare every signature.

    ctypes defaults an undeclared return type to `int`, which is 32 bits. On 64-bit builds that
    truncates every returned pointer and produces a plausible-looking non-null value that segfaults
    on use. Declaring `restype` is not optional tidiness.
    """
    p = ctypes.c_void_p
    u8p = ctypes.POINTER(ctypes.c_ubyte)
    f32p = ctypes.POINTER(ctypes.c_float)
    sig = {
        "idx_abi_version": ([], ctypes.c_uint32),
        "idx_alloc": ([ctypes.c_size_t], u8p),
        "idx_free": ([u8p, ctypes.c_size_t], None),
        "idx_build_new": ([ctypes.c_char_p, ctypes.c_size_t], p),
        "idx_build_facet": ([p, ctypes.c_uint32], ctypes.c_uint32),
        "idx_build_add": ([p, ctypes.c_char_p, ctypes.c_size_t], ctypes.c_uint32),
        "idx_build_finish": ([p], p),
        "idx_build_free": ([p], None),
        "idx_open": ([ctypes.c_char_p, ctypes.c_size_t], p),
        "idx_serialize": ([p], ctypes.c_size_t),
        "idx_close": ([p], None),
        "idx_doc_count": ([p], ctypes.c_uint32),
        "idx_term_count": ([p], ctypes.c_uint32),
        "idx_search": ([p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32], ctypes.c_uint32),
        "idx_result_ptr": ([p], u8p),
        "idx_result_len": ([p], ctypes.c_size_t),
        "idx_search_facet": (
            [
                p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32,
                ctypes.c_char_p, ctypes.c_size_t,
            ],
            ctypes.c_uint32,
        ),
        "idx_search_facet_all": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_char_p, ctypes.c_size_t],
            ctypes.c_uint32,
        ),
        "idx_facet_tally": ([p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32], ctypes.c_uint32),
        "idx_facet_count": ([p, ctypes.c_uint32], ctypes.c_uint32),
        "idx_facet_slot_count": ([p], ctypes.c_uint32),
        "idx_build_numeric": ([p, ctypes.c_uint32], ctypes.c_uint32),
        "idx_search_range": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32,
             ctypes.c_double, ctypes.c_double],
            ctypes.c_uint32,
        ),
        "idx_range_tally": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32,
             ctypes.POINTER(ctypes.c_double), ctypes.c_size_t],
            ctypes.c_uint32,
        ),
        "idx_numeric_slot_count": ([p], ctypes.c_uint32),
        "idx_searcher_new": ([p], p),
        "idx_searcher_push": ([p, p], ctypes.c_uint32),
        "idx_searcher_close": ([p], None),
        "idx_searcher_doc_count": ([p], ctypes.c_uint32),
        "idx_searcher_segment_count": ([p], ctypes.c_uint32),
        "idx_searcher_live_count": ([p], ctypes.c_uint32),
        "idx_searcher_needs_compaction": ([p], ctypes.c_uint32),
        "idx_searcher_delete": ([p, ctypes.c_uint32], ctypes.c_uint32),
        "idx_searcher_search": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32],
            ctypes.c_uint32,
        ),
        "idx_searcher_facet_tally": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32], ctypes.c_uint32
        ),
        "idx_searcher_result_ptr": ([p], u8p),
        "idx_searcher_result_len": ([p], ctypes.c_size_t),
        "idx_highlight": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p, ctypes.c_size_t],
            ctypes.c_uint32,
        ),
        "idx_search_sorted": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32,
             ctypes.c_uint32],
            ctypes.c_uint32,
        ),
        "idx_search_clause": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32,
             ctypes.c_char_p, ctypes.c_size_t],
            ctypes.c_uint32,
        ),
        "idx_search_page": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32],
            ctypes.c_uint32,
        ),
        "idx_build_position": ([p], ctypes.c_uint32),
        "idx_search_phrase": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32],
            ctypes.c_uint32,
        ),
        "idx_searcher_search_phrase": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32],
            ctypes.c_uint32,
        ),
        "idx_searcher_search_clause": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32,
             ctypes.c_char_p, ctypes.c_size_t],
            ctypes.c_uint32,
        ),
        "idx_searcher_search_page": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32],
            ctypes.c_uint32,
        ),
        "idx_build_key": ([p, ctypes.c_uint32], ctypes.c_uint32),
        "idx_doc_of_key": ([p, ctypes.c_char_p, ctypes.c_size_t], ctypes.c_uint32),
        "idx_key_of": ([p, ctypes.c_uint32], ctypes.c_size_t),
        "idx_keyed_count": ([p], ctypes.c_uint32),
        "idx_searcher_doc_of_key": ([p, ctypes.c_char_p, ctypes.c_size_t], ctypes.c_uint32),
        "idx_searcher_delete_key": ([p, ctypes.c_char_p, ctypes.c_size_t], ctypes.c_uint32),
        "idx_searcher_key_of": ([p, ctypes.c_uint32], ctypes.c_size_t),
        "idx_searcher_has_key": ([p], ctypes.c_uint32),
        # -- the image tier. `f32p` is the whole point of the row: an embedding crosses as a
        # pointer to floats the host already owns, not as text that must be parsed on arrival.
        "idx_image_new": ([ctypes.c_char_p, ctypes.c_size_t, ctypes.c_size_t, ctypes.c_uint32], p),
        "idx_image_push_vector": ([p, f32p, ctypes.c_size_t], ctypes.c_uint32),
        "idx_image_add": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint32,
             ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32,
             ctypes.c_char_p, ctypes.c_size_t],
            ctypes.c_uint32,
        ),
        "idx_image_build": ([p], ctypes.c_uint32),
        "idx_image_doc_count": ([p], ctypes.c_uint32),
        "idx_image_search_vector": (
            [p, f32p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32], ctypes.c_uint32
        ),
        "idx_image_search_fused": (
            [p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p, ctypes.c_size_t,
             ctypes.POINTER(ctypes.c_double), ctypes.c_size_t,
             f32p, ctypes.c_size_t,
             ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32,
             ctypes.c_float, ctypes.c_uint32],
            ctypes.c_uint32,
        ),
        "idx_image_hash_near": (
            [p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32], ctypes.c_uint32
        ),
        "idx_image_why": ([p, ctypes.c_uint32], ctypes.c_uint32),
        "idx_image_result_ptr": ([p], u8p),
        "idx_image_result_len": ([p], ctypes.c_size_t),
        "idx_image_free": ([p], None),
    }
    for name, (argtype, restype) in sig.items():
        fn = getattr(lib, name)
        fn.argtypes = argtype
        fn.restype = restype
    return lib


class Clause:
    """One entry in a filter bar: **OR within a clause, AND across clauses**, and NOT.

    That is not an arbitrary choice -- it is how a filter bar behaves. Ticking two brands widens
    the result; ticking a brand and a category narrows it.

    An unknown value is ignored rather than fatal, because a bar built from one segment's labels
    may legitimately name a value another segment has never seen. The two ends of that rule are
    deliberately opposite, and reversing them is how a filter silently returns the whole corpus:

    * an **include** whose values are all unknown matches **nothing** -- nobody can satisfy it;
    * an **exclude** whose values are all unknown excludes **nothing** -- there is nothing to remove.

    The same asymmetry governs a row with no value in the slot: kept by an exclude, dropped by an
    include, so an unbranded row survives *"not Colgate"*.
    """

    __slots__ = ("slot", "value", "exclude")

    def __init__(self, slot: int, value: list[str], exclude: bool = False) -> None:
        self.slot = slot
        self.value = value
        self.exclude = exclude

    @staticmethod
    def any(slot: int, value: list[str]) -> "Clause":
        """Satisfied by any of `value`."""
        return Clause(slot, value, False)

    @staticmethod
    def none(slot: int, value: list[str]) -> "Clause":
        """Satisfied by rows matching none of `value`."""
        return Clause(slot, value, True)

    def __repr__(self) -> str:
        return f"Clause(slot={self.slot}, value={self.value!r}, exclude={self.exclude})"


def _clause_spec(clause: list[Clause]) -> bytes:
    """Encode clauses as the ABI's NUL-separated `slot[!]=v1|v2|...` spec.

    `|` separates alternatives, so a facet value CONTAINING a `|` cannot be expressed. That is a
    documented limit of the wire format; raising here is better than shipping a spec that silently
    means two values where the host meant one.
    """
    part = []
    for c in clause:
        for v in c.value:
            if "|" in v:
                raise ValueError(f"a facet value containing '|' cannot be expressed: {v!r}")
        bang = "!" if c.exclude else ""
        part.append(f"{c.slot}{bang}={'|'.join(c.value)}".encode())
    return SEP.join(part)


def _hit(raw: bytes, n: int) -> list["Hit"]:
    """Decode `n` packed hit records. One decoder, so a change to `HIT_BYTE` cannot half-land."""
    return [Hit(*struct.unpack_from("<IfI", raw, i * HIT_BYTE)) for i in range(n)]


class Hit:
    __slots__ = ("doc", "score", "typo_bucket")

    def __init__(self, doc: int, score: float, typo_bucket: int) -> None:
        self.doc = doc
        self.score = score
        self.typo_bucket = typo_bucket

    def __repr__(self) -> str:
        return f"Hit(doc={self.doc}, score={self.score:.4f}, typo_bucket={self.typo_bucket})"


class Live:
    """A live, appendable collection: incremental updates without rebuilding.

    Takes ownership of the `Index` objects it is given -- the C ABI consumes those handles, so the
    Python wrappers are neutered rather than left pointing at freed memory.
    """

    def __init__(self, lib: ctypes.CDLL, base: "Index") -> None:
        self._lib = lib
        self._s = lib.idx_searcher_new(base._take())
        if not self._s:
            raise ValueError("could not create searcher")

    def push(self, seg: "Index") -> None:
        if not self._lib.idx_searcher_push(self._s, seg._take()):
            raise ValueError("could not append segment")

    def search(self, query: str, k: int = 10, prefix: bool = False) -> list[Hit]:
        q = query.encode()
        n = self._lib.idx_searcher_search(self._s, q, len(q), k, 1 if prefix else 0)
        if n == U32_MAX:
            raise ValueError(f"search failed: {query!r}")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_searcher_result_ptr(self._s), n * HIT_BYTE)
        return [Hit(*struct.unpack_from("<IfI", raw, i * HIT_BYTE)) for i in range(n)]

    def search_clause(
        self, query: str, clause: list[Clause], k: int = 10, offset: int = 0
    ) -> list[Hit]:
        """The filter bar across every segment.

        The unknown-value rule of `Clause` applies PER SEGMENT: a value one segment has never
        interned is ignored there. That is what lets a bar built from one segment's labels stay
        correct against a collection holding several.
        """
        q = query.encode()
        spec = _clause_spec(clause)
        n = self._lib.idx_searcher_search_clause(self._s, q, len(q), k, offset, spec, len(spec))
        if n == U32_MAX:
            raise ValueError(f"malformed clause filter: {clause!r}")
        if n == 0:
            return []
        return _hit(ctypes.string_at(self._lib.idx_searcher_result_ptr(self._s), n * HIT_BYTE), n)

    def search_phrase(self, query: str, k: int = 10, offset: int = 0) -> list[Hit]:
        """`Index.search_phrase` across every segment.

        A segment built without positions contributes nothing, rather than contributing term
        matches that would be unidentifiable once merged with real phrase matches.
        """
        q = query.encode()
        n = self._lib.idx_searcher_search_phrase(self._s, q, len(q), offset, k)
        if n == U32_MAX:
            raise ValueError(f"phrase search failed: {query!r}")
        if n == 0:
            return []
        return _hit(ctypes.string_at(self._lib.idx_searcher_result_ptr(self._s), n * HIT_BYTE), n)

    def search_page(self, query: str, offset: int = 0, k: int = 10) -> list[Hit]:
        """`Index.search_page` across every segment.

        Cost grows with `offset` AND with segment count together: every segment must produce
        `offset + k` hits before the merge can decide which of them the page holds, because the
        page boundary is global -- a segment's 3rd-best hit can be the page's 1st.
        """
        q = query.encode()
        n = self._lib.idx_searcher_search_page(self._s, q, len(q), offset, k)
        if n == U32_MAX:
            raise ValueError(f"paged search failed: {query!r}")
        if n == 0:
            return []
        return _hit(ctypes.string_at(self._lib.idx_searcher_result_ptr(self._s), n * HIT_BYTE), n)

    def facet_tally(self, query: str, slot: int = 0) -> list[tuple[str, int]]:
        q = query.encode()
        n = self._lib.idx_searcher_facet_tally(self._s, q, len(q), slot)
        if n == U32_MAX:
            raise ValueError(f"tally failed: {query!r}")
        raw = ctypes.string_at(
            self._lib.idx_searcher_result_ptr(self._s), self._lib.idx_searcher_result_len(self._s)
        )
        out, at = [], 0
        for _ in range(n):
            (ln,) = struct.unpack_from("<I", raw, at)
            at += 4
            label = raw[at : at + ln].decode()
            at += ln
            (c,) = struct.unpack_from("<I", raw, at)
            at += 4
            out.append((label, c))
        return out

    def delete(self, global_doc: int) -> bool:
        return bool(self._lib.idx_searcher_delete(self._s, global_doc))

    def doc_of_key(self, key: str) -> int | None:
        """The LIVE document carrying `key`, as a global ordinal, or `None`.

        Segments are searched newest first: a key present in more than one means the row was
        updated, and the newest version is the live one.
        """
        k = key.encode()
        d = self._lib.idx_searcher_doc_of_key(self._s, k, len(k))
        return None if d == U32_MAX else d

    def delete_key(self, key: str) -> bool:
        """Tombstone the row carrying `key`. **This is what a change stream's delete becomes.**

        `Live.delete` cannot serve it: that takes a dense global ordinal, which is assigned at
        insertion and is not something a database row carries.

        Deleting a key that is already gone returns `False` and is not an error -- a stream replayed
        from an earlier offset re-delivers deletes, and refusing would make replay impossible.
        """
        k = key.encode()
        return bool(self._lib.idx_searcher_delete_key(self._s, k, len(k)))

    def key_of(self, global_doc: int) -> str | None:
        """The application key of a global ordinal, or `None`."""
        n = self._lib.idx_searcher_key_of(self._s, global_doc)
        if n == 0:
            return None
        return ctypes.string_at(self._lib.idx_searcher_result_ptr(self._s), n).decode()

    @property
    def has_key(self) -> bool:
        """Whether EVERY segment carries keys, so a change stream can address every row."""
        return bool(self._lib.idx_searcher_has_key(self._s))

    @property
    def doc_count(self) -> int:
        return self._lib.idx_searcher_doc_count(self._s)

    @property
    def segment_count(self) -> int:
        return self._lib.idx_searcher_segment_count(self._s)

    @property
    def live_count(self) -> int:
        return self._lib.idx_searcher_live_count(self._s)

    @property
    def needs_compaction(self) -> bool:
        return bool(self._lib.idx_searcher_needs_compaction(self._s))

    def close(self) -> None:
        if self._s:
            self._lib.idx_searcher_close(self._s)
            self._s = 0

    def __enter__(self) -> "Live":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class Index:
    """A live index. Use as a context manager so the handle is always closed."""

    def __init__(self, lib: ctypes.CDLL, handle: int) -> None:
        if not handle:
            raise ValueError("null handle")
        self._lib = lib
        self._h = handle

    # -- construction ---------------------------------------------------------------------
    @staticmethod
    def build(
        lib: ctypes.CDLL,
        field: list[tuple[str, float, float]],
        row: list[list[str]],
        facet: int | list[int] | None = None,
        numeric: int | list[int] | None = None,
        position: bool = False,
        key: int | None = None,
    ) -> "Index":
        spec = SEP.join(f"{n}:{boost}:{b}".encode() for n, boost, b in field)
        builder = lib.idx_build_new(spec, len(spec))
        if not builder:
            raise ValueError(f"malformed field spec: {spec!r}")
        for f in [facet] if isinstance(facet, int) else (facet or []):
            if not lib.idx_build_facet(builder, f):
                lib.idx_build_free(builder)
                raise ValueError(f"facet field out of range: {f}")
        for f in [numeric] if isinstance(numeric, int) else (numeric or []):
            if not lib.idx_build_numeric(builder, f):
                lib.idx_build_free(builder)
                raise ValueError(f"numeric field out of range: {f}")
        if position and not lib.idx_build_position(builder):
            lib.idx_build_free(builder)
            raise ValueError("positions must be enabled before the first document")
        if key is not None and not lib.idx_build_key(builder, key):
            lib.idx_build_free(builder)
            raise ValueError(f"key field out of range or set too late: {key}")
        try:
            for r in row:
                blob = SEP.join(v.encode() for v in r)
                if lib.idx_build_add(builder, blob, len(blob)) == U32_MAX:
                    raise ValueError(f"document rejected: {r!r}")
        except Exception:
            lib.idx_build_free(builder)
            raise
        # Consumes the builder -- no idx_build_free after this, success or failure.
        handle = lib.idx_build_finish(builder)
        if not handle:
            raise ValueError("build failed")
        return Index(lib, handle)

    @staticmethod
    def open(lib: ctypes.CDLL, blob: bytes) -> "Index":
        handle = lib.idx_open(blob, len(blob))
        if not handle:
            raise ValueError("malformed index bytes")
        return Index(lib, handle)

    # -- use ------------------------------------------------------------------------------
    def search(self, query: str, k: int = 10, prefix: bool = False) -> list[Hit]:
        q = query.encode()
        n = self._lib.idx_search(self._h, q, len(q), k, 1 if prefix else 0)
        if n == U32_MAX:
            raise ValueError(f"search failed: {query!r}")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE)
        return [Hit(*struct.unpack_from("<IfI", raw, i * HIT_BYTE)) for i in range(n)]

    def search_facet(self, query: str, value: str, k: int = 10, slot: int = 0) -> list[Hit]:
        """Filter-then-rank: `k` hits from inside `value`, not `k` global hits that survived it."""
        q, v = query.encode(), value.encode()
        n = self._lib.idx_search_facet(self._h, q, len(q), k, slot, v, len(v))
        if n == U32_MAX:
            raise ValueError(f"faceted search failed: {query!r} in {value!r}")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE)
        return [Hit(*struct.unpack_from("<IfI", raw, i * HIT_BYTE)) for i in range(n)]

    def search_facet_all(self, query: str, want: list[tuple[int, str]], k: int = 10) -> list[Hit]:
        """Conjunctive: every (slot, value) pair must hold. Brand AND category, in one pass."""
        q = query.encode()
        spec = SEP.join(f"{slot}={value}".encode() for slot, value in want)
        n = self._lib.idx_search_facet_all(self._h, q, len(q), k, spec, len(spec))
        if n == U32_MAX:
            raise ValueError(f"malformed facet filter: {want!r}")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE)
        return [Hit(*struct.unpack_from("<IfI", raw, i * HIT_BYTE)) for i in range(n)]

    def search_range(self, query: str, lo: float, hi: float, k: int = 10, slot: int = 0) -> list[Hit]:
        """Half-open `lo <= value < hi`, so adjacent buckets never both claim the boundary."""
        q = query.encode()
        n = self._lib.idx_search_range(self._h, q, len(q), k, slot, lo, hi)
        if n == U32_MAX:
            raise ValueError(f"range search failed: {query!r}")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE)
        return [Hit(*struct.unpack_from("<IfI", raw, i * HIT_BYTE)) for i in range(n)]

    def range_tally(self, query: str, edge: list[float], slot: int = 0) -> list[int]:
        """Histogram over EVERY matching document. Counts sum to at most the match count:
        a document with no value in the column is counted in no bucket."""
        q = query.encode()
        arr = (ctypes.c_double * len(edge))(*edge)
        n = self._lib.idx_range_tally(self._h, q, len(q), slot, arr, len(edge))
        if n == U32_MAX:
            raise ValueError(f"range tally failed: {query!r}")
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), n * 4)
        return list(struct.unpack(f"<{n}I", raw))

    def search_sorted(self, query: str, ascending: bool = True, k: int = 10, slot: int = 0) -> list[Hit]:
        """Order by a numeric column rather than relevance. Cannot prune: cost tracks the
        query's match count, not k. Rows with no value in the column are excluded."""
        q = query.encode()
        n = self._lib.idx_search_sorted(self._h, q, len(q), k, slot, 1 if ascending else 0)
        if n == U32_MAX:
            raise ValueError(f"sorted search failed: {query!r}")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE)
        return [Hit(*struct.unpack_from("<IfI", raw, i * HIT_BYTE)) for i in range(n)]

    def search_clause(
        self, query: str, clause: list[Clause], k: int = 10, offset: int = 0
    ) -> list[Hit]:
        """The whole filter bar: OR within a clause, AND across clauses, NOT, and an offset.

        See `Clause` for the unknown-value rule, which is asserted in both directions.
        """
        q = query.encode()
        spec = _clause_spec(clause)
        n = self._lib.idx_search_clause(self._h, q, len(q), k, offset, spec, len(spec))
        if n == U32_MAX:
            raise ValueError(f"malformed clause filter: {clause!r}")
        if n == 0:
            return []
        return _hit(ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE), n)

    def doc_of_key(self, key: str) -> int | None:
        """The document carrying `key`, or `None`.

        A DELETED document is still found: this answers "which ordinal is this row", which a caller
        needs precisely in order to delete it.
        """
        k = key.encode()
        d = self._lib.idx_doc_of_key(self._h, k, len(k))
        return None if d == U32_MAX else d

    def key_of(self, doc: int) -> str | None:
        """The application key of `doc`, or `None` when the row has none."""
        n = self._lib.idx_key_of(self._h, doc)
        if n == 0:
            return None
        return ctypes.string_at(self._lib.idx_result_ptr(self._h), n).decode()

    @property
    def keyed_count(self) -> int:
        """Rows carrying a key. Below `doc_count` when some key fields were blank -- and those rows
        can never be addressed by a change stream."""
        return self._lib.idx_keyed_count(self._h)

    def search_phrase(self, query: str, k: int = 10, offset: int = 0) -> list[Hit]:
        """The query's tokens, consecutive and in order, within ONE field.

        Requires `Index.build(..., position=True)`. Without positions this returns `[]` and does
        **not** fall back to an ordinary term search -- a fallback would hand back bag-of-words rows
        that are indistinguishable from phrase rows.

        A phrase is **exact**: no token is typo-corrected or prefix-expanded, because quoting is the
        caller asserting these words. A token absent from the dictionary matches nothing.

        Ranking is unchanged; the phrase is a filter applied after scoring, like a facet clause.
        """
        q = query.encode()
        n = self._lib.idx_search_phrase(self._h, q, len(q), offset, k)
        if n == U32_MAX:
            raise ValueError(f"phrase search failed: {query!r}")
        if n == 0:
            return []
        return _hit(ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE), n)

    def search_page(self, query: str, offset: int = 0, k: int = 10) -> list[Hit]:
        """Page `n` is `offset = n * k`.

        **Cost grows with `offset`, not with `k`**: the engine over-fetches `offset + k` and drops
        the prefix, because rank order is only known once everything above the page is scored.
        Every engine without a stored cursor works this way. For deep paging, filter instead.
        """
        q = query.encode()
        n = self._lib.idx_search_page(self._h, q, len(q), offset, k)
        if n == U32_MAX:
            raise ValueError(f"paged search failed: {query!r}")
        if n == 0:
            return []
        return _hit(ctypes.string_at(self._lib.idx_result_ptr(self._h), n * HIT_BYTE), n)

    @property
    def numeric_slot_count(self) -> int:
        return self._lib.idx_numeric_slot_count(self._h)

    def facet_tally(self, query: str, slot: int = 0) -> list[tuple[str, int]]:
        """Counts over EVERY matching document, not the top k. Sorted by count descending.

        Wire format is `u32 len, UTF-8 bytes, u32 count` per entry -- length-prefixed rather than
        NUL-separated, because a facet value may legitimately contain anything but a NUL and this
        format need not assume even that.
        """
        q = query.encode()
        n = self._lib.idx_facet_tally(self._h, q, len(q), slot)
        if n == U32_MAX:
            raise ValueError(f"tally failed: {query!r}")
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), self._lib.idx_result_len(self._h))
        out, at = [], 0
        for _ in range(n):
            (ln,) = struct.unpack_from("<I", raw, at)
            at += 4
            label = raw[at : at + ln].decode()
            at += ln
            (count,) = struct.unpack_from("<I", raw, at)
            at += 4
            out.append((label, count))
        if at != len(raw):
            raise ValueError("tally encoding is not self-delimiting")
        return out

    def facet_count(self, slot: int = 0) -> int:
        return self._lib.idx_facet_count(self._h, slot)

    @property
    def facet_slot_count(self) -> int:
        return self._lib.idx_facet_slot_count(self._h)

    def highlight(self, query: str, text: str) -> list[str]:
        """The substrings of `text` that matched `query`.

        Returns the matched fragments rather than raw offsets, because the ABI hands back BYTE
        offsets and Python strings are indexed by character -- slicing `text` with them directly is
        wrong the moment the text is not ASCII. Decoding from the encoded bytes is correct for any
        input, which is why this wrapper does it rather than leaving it to the caller.
        """
        q, t = query.encode(), text.encode()
        n = self._lib.idx_highlight(self._h, q, len(q), t, len(t))
        if n == U32_MAX:
            raise ValueError(f"highlight failed: {query!r}")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_result_ptr(self._h), n * 8)
        out = []
        for i in range(n):
            a, b = struct.unpack_from("<II", raw, i * 8)
            out.append(t[a:b].decode())
        return out

    def to_bytes(self) -> bytes:
        n = self._lib.idx_serialize(self._h)
        if n == 0:
            raise ValueError("serialize failed")
        # Copy out immediately: the buffer is invalidated by the next search or serialize.
        return ctypes.string_at(self._lib.idx_result_ptr(self._h), n)

    @property
    def doc_count(self) -> int:
        return self._lib.idx_doc_count(self._h)

    @property
    def term_count(self) -> int:
        return self._lib.idx_term_count(self._h)

    def _take(self) -> int:
        """Hand the raw handle to something that CONSUMES it, and forget it here.

        Without this the Python object would still close a handle the searcher now owns --
        a double free, which is the one memory bug a ctypes host can still write."""
        h, self._h = self._h, 0
        return h

    def close(self) -> None:
        if self._h:
            self._lib.idx_close(self._h)
            self._h = 0

    def __enter__(self) -> "Index":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


# ---- The image tier -------------------------------------------------------------------------

IMAGE_HIT_BYTE = 12  # doc u32, score f32, why u32 -- the same width as a text hit, `why` where
IMAGE_NEAR_BYTE = 8  # `typo_bucket` sits, so one decoder is renamed rather than written twice.

WHY_TEXT = 1
WHY_VECTOR = 2
WHY_HASH = 4
WHY_COLOR = 8  # reserved; no arm sets it yet

METRIC_COSINE = 0
METRIC_DOT = 1
METRIC_L2 = 2


def _f32(value: list[float]) -> ctypes.Array:
    """Pack floats as the flat little-endian `f32` buffer the ABI reads.

    THE EMBEDDING NEVER CROSSES AS JSON. A 512-d embedding is 2 KB raw and roughly 6 KB as JSON
    text that must then be parsed; at the ingest rates this tier is built for the serialise/parse
    pair would dominate the boundary and buy nothing, because both sides already agree on
    IEEE-754 little-endian and the copy IS the whole conversion.

    `struct.pack` does that copy in the standard library -- no numpy, no extension module, no
    build step in the host. That is the claim this file exists to make.
    """
    if sys.byteorder != "little":
        raise RuntimeError("the ABI's float buffers are little-endian; this host is not")
    return (ctypes.c_float * len(value)).from_buffer_copy(struct.pack(f"<{len(value)}f", *value))


class ImageHit:
    __slots__ = ("doc", "score", "why")

    def __init__(self, doc: int, score: float, why: int) -> None:
        self.doc = doc
        self.score = score
        self.why = why

    def __repr__(self) -> str:
        return f"ImageHit(doc={self.doc}, score={self.score:.4f}, why={self.why})"


class Near:
    __slots__ = ("doc", "distance")

    def __init__(self, doc: int, distance: int) -> None:
        self.doc = doc
        self.distance = distance

    def __repr__(self) -> str:
        return f"Near(doc={self.doc}, distance={self.distance})"


class ImageIndex:
    """An image index: caption, facet, numeric column, embedding and perceptual hash in ONE
    document set, answered by ONE query plan that selects top-k exactly once.

    One handle covers both lifetimes -- builder and index -- so the ordering rule is enforced by
    the sentinels rather than by documentation: queries refuse before `build()`, mutations after.
    """

    def __init__(
        self,
        lib: ctypes.CDLL,
        field: list[str],
        dim: int,
        metric: int = METRIC_COSINE,
    ) -> None:
        spec = SEP.join(f.encode() for f in field)
        self._lib = lib
        self._h = lib.idx_image_new(spec, len(spec), dim, metric)
        if not self._h:
            raise ValueError(f"malformed image spec, dim or metric: {spec!r}")
        self._dim = dim
        self._built = False

    def push_vector(self, vec: list[float]) -> bool:
        """Stage the embedding the NEXT `add` attaches.

        Returns False rather than raising on a dimension mismatch, because that is the sentinel
        the ABI defines and a host must be able to see it. The mismatch means a model was swapped
        mid-run, and it is refused at the call that can name both numbers.
        """
        buf = _f32(vec)
        return bool(self._lib.idx_image_push_vector(self._h, buf, len(vec)))

    def add(
        self,
        value: list[str],
        dhash: int | None = None,
        phash: int | None = None,
        digest: bytes | None = None,
    ) -> int:
        """Add one image. Returns its ordinal, or raises.

        A 64-bit hash crosses as two `u32` halves -- `present` says which hashes are real, because
        an absent hash is genuinely absent and a zero hash is a legitimate all-dark image.
        """
        blob = SEP.join(v.encode() for v in value)
        present = (1 if dhash is not None else 0) | (2 if phash is not None else 0)
        d_hi, d_lo = ((dhash >> 32) & U32_MAX, dhash & U32_MAX) if dhash is not None else (0, 0)
        p_hi, p_lo = ((phash >> 32) & U32_MAX, phash & U32_MAX) if phash is not None else (0, 0)
        ord_ = self._lib.idx_image_add(
            self._h, blob, len(blob), present, d_hi, d_lo, p_hi, p_lo,
            digest, len(digest) if digest else 0,
        )
        if ord_ == U32_MAX:
            raise ValueError(f"image rejected: {value!r}")
        return ord_

    def build(self) -> bool:
        ok = bool(self._lib.idx_image_build(self._h))
        self._built = self._built or ok
        return ok

    # -- queries ---------------------------------------------------------------------------
    def _hit(self, n: int) -> list[ImageHit]:
        if n == U32_MAX:
            raise ValueError("image search failed")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_image_result_ptr(self._h), n * IMAGE_HIT_BYTE)
        return [
            ImageHit(*struct.unpack_from("<IfI", raw, i * IMAGE_HIT_BYTE)) for i in range(n)
        ]

    def search_vector(self, vec: list[float], k: int = 10, oversample: int = 4) -> list[ImageHit]:
        """Vector alone: binary popcount shortlist, int8 rerank, exact final pass.

        APPLIES NO FILTER, which is exactly why it is a separate entry point from `search_fused`:
        a host wanting a filtered vector search has to ask for one, so the filter cannot be lost
        by accident.
        """
        buf = _f32(vec)
        return self._hit(self._lib.idx_image_search_vector(self._h, buf, len(vec), k, oversample))

    def search_fused(
        self,
        query: str | None = None,
        clause: list[Clause] | None = None,
        range_: list[tuple[int, float, float]] | None = None,
        vec: list[float] | None = None,
        dhash: int | None = None,
        hash_max: int = 4,
        alpha: float = -1.0,
        k: int = 10,
    ) -> list[ImageHit]:
        """THE FUSED QUERY: text, facets, ranges, vector proximity and hash proximity, scored
        together, top-k selected ONCE.

        The claim is not speed. It is that a hard predicate is hard EVERYWHERE -- the facet and
        range clauses are re-applied to the vector and hash arms rather than trusted -- and that
        no arm's own top-k can hide a document from the fused answer. A fan-out over a separate
        vector store cannot have either property, and `docs/research/image.md` §1 records that
        every alive FOSS photo system is exactly such a fan-out.

        `alpha` negative means "use the default", which is 0.5 and asserts nothing about which
        signal is better -- the honest position before a labelled judgment set exists.
        """
        q = query.encode() if query else None
        spec = _clause_spec(clause) if clause else None
        flat: list[float] = []
        for slot, lo, hi in range_ or []:
            flat += [float(slot), lo, hi]
        edge = (ctypes.c_double * len(flat))(*flat) if flat else None
        buf = _f32(vec) if vec else None
        hi_, lo_ = ((dhash >> 32) & U32_MAX, dhash & U32_MAX) if dhash is not None else (0, 0)
        n = self._lib.idx_image_search_fused(
            self._h,
            q, len(q) if q else 0,
            spec, len(spec) if spec else 0,
            edge, len(flat) // 3,
            buf, len(vec) if vec else 0,
            1 if dhash is not None else 0, hi_, lo_, hash_max,
            alpha, k,
        )
        return self._hit(n)

    def hash_near(self, dhash: int, max_distance: int = 4) -> list[Near]:
        """Near-duplicates of a dHash probe, ordered by ascending distance then ordinal.

        This writes to the SAME buffer the searches use in a DIFFERENT record layout, so the
        returned count is what tells a host which decoder to run. A document with no dHash appears
        in no result at all.
        """
        n = self._lib.idx_image_hash_near(
            self._h, (dhash >> 32) & U32_MAX, dhash & U32_MAX, max_distance
        )
        if n == U32_MAX:
            raise ValueError("hash_near failed")
        if n == 0:
            return []
        raw = ctypes.string_at(self._lib.idx_image_result_ptr(self._h), n * IMAGE_NEAR_BYTE)
        return [Near(*struct.unpack_from("<II", raw, i * IMAGE_NEAR_BYTE)) for i in range(n)]

    def why(self, hit: int) -> int:
        """Which signal found hit `hit`: a bitmask of WHY_*, or U32_MAX past the end.

        Never 0 for an error -- 0 is a legitimate mask meaning "no arm claimed it". This exists
        for exactly the host this file is: one number about one row, without writing a decoder.
        """
        return self._lib.idx_image_why(self._h, hit)

    @property
    def doc_count(self) -> int:
        return self._lib.idx_image_doc_count(self._h)

    def close(self) -> None:
        if self._h:
            self._lib.idx_image_free(self._h)
            self._h = 0

    def __enter__(self) -> "ImageIndex":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


# ---- Self-test ------------------------------------------------------------------------------
# Mirrors js/smoke.mjs assertion for assertion, so a divergence between the WASM and native tiers
# shows up as one tier failing a check the other passes.

ROW = [
    ["Colgate Total Toothpaste 150g", "Colgate"],
    ["Safeguard Pure White Soap 135g", "Safeguard"],
    ["Lucky Me Pancit Canton 60g", "Lucky Me"],
    ["Bear Brand Fortified Milk 320g", "Bear Brand"],
]


# The six-image corpus of `js/image-smoke.mjs`, verbatim. `camera` is a facet, `year` a numeric
# column, and the embeddings are hand-picked so the probe's NEAREST neighbour (doc 1, an iPhone
# shot) is EXCLUDED by the Canon filter used below. That is what makes the leak test falsifiable
# rather than decorative: if the vector arm were trusted instead of filtered, doc 1 would rank
# first, and it is the only arrangement in which that failure is visible.
IMAGE_DIM = 8
IMAGE_FIELD = ["caption:3:0.4", "camera:1:0.6:f", "year:0:0.6:n"]
IMAGE_ROW = [
    ("beach sunset waves",  "Canon EOS R5", "2019",
     [0.90, 0.20, 0.10, 0.00, 0.00, 0.10, 0.00, 0.00], 0x0123456789ABCDEF),
    ("beach umbrella sand", "iPhone 15",    "2020",
     [0.95, 0.25, 0.05, 0.00, 0.00, 0.00, 0.00, 0.00], 0x0123456789ABCDEC),
    ("mountain snow peak",  "Canon EOS R5", "2021",
     [0.10, 0.90, 0.20, 0.00, 0.00, 0.00, 0.00, 0.00], 0xFFFFFFFFFFFFFFFF),
    ("city street night",   "iPhone 15",    "2018",
     [0.00, 0.10, 0.95, 0.20, 0.00, 0.00, 0.00, 0.00], 0x0000000000000000),
    ("beach dog running",   "Pixel 8",      "2022",
     [0.80, 0.30, 0.20, 0.10, 0.00, 0.00, 0.00, 0.00], 0xAAAAAAAAAAAAAAAA),
    ("forest trail mist",   "Canon EOS R5", "2017",
     [0.20, 0.20, 0.90, 0.10, 0.00, 0.00, 0.00, 0.00], 0x5555555555555555),
]
IMAGE_PROBE = [1.0, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]


def image_check(lib: ctypes.CDLL, check) -> None:
    """The image tier through `ctypes`, in the standard library alone."""
    import math

    def cosine(a: list[float], b: list[float]) -> float:
        dot = sum(x * y for x, y in zip(a, b))
        return dot / math.sqrt(sum(x * x for x in a) * sum(y * y for y in b))

    # The expected order is DERIVED here, not copied from a run: the column L2-normalises on
    # ingest and the query on search, so cosine IS the score, and the min-max normalisation inside
    # the fusion is monotone and cannot reorder a single arm.
    by_cosine = [
        d for d, _ in sorted(
            ((i, cosine(r[3], IMAGE_PROBE)) for i, r in enumerate(IMAGE_ROW)),
            key=lambda t: (-t[1], t[0]),
        )
    ]

    # A packed f32 buffer really is 4 bytes per component, little-endian, and nothing else.
    packed = struct.pack(f"<{IMAGE_DIM}f", *IMAGE_PROBE)
    check(len(packed) == IMAGE_DIM * 4, "an embedding packs to 4 bytes per component, no framing")
    check(packed[:4] == b"\x00\x00\x80\x3f", "1.0 packs as the little-endian IEEE-754 f32 3f800000")

    check(ImageIndex(lib, IMAGE_FIELD, IMAGE_DIM).doc_count == 0, "a fresh image index is empty")
    try:
        ImageIndex(lib, ["caption:3:0.4", "camera:1:0.6:X"], IMAGE_DIM)
        check(False, "an unrecognised column type is refused, not ignored")
    except ValueError:
        check(True, "an unrecognised column type is refused, not ignored")
    try:
        ImageIndex(lib, IMAGE_FIELD, IMAGE_DIM, metric=99)
        check(False, "an unknown metric is refused")
    except ValueError:
        check(True, "an unknown metric is refused")

    with ImageIndex(lib, IMAGE_FIELD, IMAGE_DIM, METRIC_COSINE) as ix:
        for caption, camera, year, vec, dhash in IMAGE_ROW:
            check(ix.push_vector(vec), f'embedding staged for "{caption}"')
            ix.add([caption, camera, year], dhash=dhash)

        # A model swapped mid-run: refused at the call that can name both numbers, never a trap.
        check(not ix.push_vector([1.0, 2.0, 3.0]), "a dimension mismatch is refused, not trapped")
        try:
            ix.search_vector(IMAGE_PROBE, k=5)
            check(False, "a query before build is refused")
        except ValueError:
            check(True, "a query before build is refused")

        check(ix.build(), "idx_image_build finishes the index")
        check(not ix.build(), "building twice is refused rather than trapping")
        check(ix.doc_count == len(IMAGE_ROW), "doc_count matches what was added")

        hit = ix.search_vector(IMAGE_PROBE, k=len(IMAGE_ROW))
        check(
            [h.doc for h in hit] == by_cosine,
            f"vector search matches the ordering Python computed: {by_cosine}",
        )
        check(
            all(abs(h.score - cosine(IMAGE_ROW[h.doc][3], IMAGE_PROBE)) < 1e-5 for h in hit),
            "and its scores are the cosine similarities, not a rescaling",
        )
        check(all(h.why == WHY_VECTOR for h in hit), "every vector-only hit is attributed to the vector arm")
        check(ix.why(0) == WHY_VECTOR, "idx_image_why agrees with the record it decoded")
        check(ix.why(99) == U32_MAX, "why past the end is the sentinel, not the empty mask 0")

        # THE FUSED QUERY, filtered. The nearest neighbour overall is an iPhone shot; the filter
        # says Canon. If the vector arm leaked the facet, doc 1 would be first.
        canon = [i for i, r in enumerate(IMAGE_ROW) if r[1] == "Canon EOS R5"]
        expect = [d for d in by_cosine if d in canon]
        check(by_cosine[0] not in canon,
              f"the nearest neighbour overall (doc {by_cosine[0]}) is EXCLUDED by the filter")
        fused = ix.search_fused(
            clause=[Clause.any(0, ["Canon EOS R5"])], vec=IMAGE_PROBE, k=len(IMAGE_ROW)
        )
        check(len(fused) == len(canon), f"the filtered fused query returns only the {len(canon)} Canon rows")
        check(all(h.doc in canon for h in fused),
              "THE VECTOR ARM DOES NOT LEAK THE FACET FILTER -- no excluded document at any score")
        check([h.doc for h in fused] == expect,
              f"and the surviving order is the cosine order restricted to the facet: {expect}")

        # A malformed spec must be an ERROR, never a silent match-everything: that is the
        # dangerous reading, and it is the one a filter bar would ship. Passed raw, because the
        # typed `Clause` cannot express a slot that is not a number -- which is the point of
        # having the type, and the reason the ABI still has to refuse the untyped case.
        bad = b"notanumber=Canon EOS R5"
        check(
            lib.idx_image_search_fused(
                ix._h, None, 0, bad, len(bad), None, 0, _f32(IMAGE_PROBE), IMAGE_DIM,
                0, 0, 0, 0, -1.0, 5,
            ) == U32_MAX,
            "a malformed filter spec errors rather than matching everything",
        )

        # Text AND vector: a row both arms found carries both bits, or explicability has not
        # survived the boundary and the fused plan's advantage is invisible to the application.
        both = ix.search_fused(query="beach", vec=IMAGE_PROBE, k=len(IMAGE_ROW))
        check(both[0].why == (WHY_TEXT | WHY_VECTOR),
              "a two-signal row ranks first -- agreement is evidence, not noise")
        check(all(ix.why(i) == h.why for i, h in enumerate(both)),
              "idx_image_why agrees with every decoded record")
        text_only = ix.search_fused(query="mountain", k=5)
        check(text_only and text_only[0].doc == 2 and text_only[0].why == WHY_TEXT,
              "a text-only fused query is attributed to the text arm alone")

        # The numeric half of the filter bar, half-open lo <= v < hi, as one flat double buffer.
        year = ix.search_fused(range_=[(0, 2019.0, 2021.0)], vec=IMAGE_PROBE, k=len(IMAGE_ROW))
        check(sorted(h.doc for h in year) == [0, 1],
              "a half-open year range excludes the row sitting exactly on the upper bound")

        # Near-duplicates. Doc 1's dHash differs from doc 0's in exactly two bits -- the
        # re-encode/resize case the column exists for. A different record layout, so the count is
        # what tells the host which decoder to run.
        near = ix.hash_near(IMAGE_ROW[0][4], 4)
        check([n.doc for n in near] == [0, 1], "hash_near finds the re-encode within radius 4")
        check([n.distance for n in near] == [0, 2], "and reports the true Hamming distances")
        check(len(ix.hash_near(IMAGE_ROW[0][4], 0)) == 1, "radius 0 is the exact hash only")

    # Nothing traps across the boundary: a freed handle is freed once, and freeing again is a
    # no-op rather than a double free -- the one memory bug a ctypes host can still write.
    lib.idx_image_free(None)
    check(True, "freeing a null image handle is a no-op, not a trap")


def main() -> int:
    lib = _bind(_load())
    failed = 0

    def check(ok: bool, msg: str) -> None:
        nonlocal failed
        print(f"  {'PASS' if ok else 'FAIL'}  {msg}")
        if not ok:
            failed += 1

    check(lib.idx_abi_version() == 13, "ABI version is 13")

    with Index.build(lib, [("name", 3, 0.4), ("brand", 1, 0.6)], ROW) as idx:
        check(idx.doc_count == len(ROW), "doc_count matches what was added")
        check(idx.term_count > 0, "the term dictionary is non-empty")

        hit = idx.search("Colgate Toothpaste", 5)
        check(len(hit) > 0, "an exact query returns hits")
        check(hit[0].doc == 0, "the Colgate document ranks first on an exact query")

        typo = idx.search("Colgte Toothpaest", 5)
        check(len(typo) > 0, "a query with two typos still returns hits")
        check(typo[0].doc == 0, "the Colgate document still ranks first with two typos")

        other = idx.search("Pancit Canton", 5)
        check(len(other) > 0 and other[0].doc == 2, "a different query ranks its own document first")

        check(
            all(a.typo_bucket <= b.typo_bucket for a, b in zip(typo, typo[1:])),
            "hits are ordered by typo_bucket first",
        )

        blob = idx.to_bytes()
        check(len(blob) > 0, "idx_serialize returns bytes")

        with Index.open(lib, blob) as reopened:
            check(reopened.doc_count == len(ROW), "the reopened index has the same doc count")
            again = reopened.search("Colgte Toothpaest", 5)
            check(
                len(again) == len(typo) and again[0].doc == 0,
                "the reopened index answers the typo query identically",
            )

    # Faceting, mirroring js/smoke.mjs check for check.
    facet_row = ROW + [["Colgate Fresh Gel 100g", "Colgate"]]
    with Index.build(lib, [("name", 3, 0.4), ("brand", 1, 0.6)], facet_row, facet=1) as fx:
        check(fx.facet_slot_count == 1, "one facet slot")
        check(fx.facet_count(0) == 4, "four distinct brands stored")
        check(len(fx.search_facet("Colgate", "Colgate")) == 2, "filtered search returns both Colgate rows")
        check(
            fx.search_facet("Colgate", "Nestle") == [],
            "an unknown facet value returns nothing, not everything",
        )
        tally = fx.facet_tally("Colgate")
        check(tally[:1] == [("Colgate", 2)], "Colgate tallies 2, ranked first")
        check(
            all(a[1] >= b[1] for a, b in zip(tally, tally[1:])),
            "the tally is sorted by count descending",
        )

    # Two slots and a conjunction: brand AND category, which is what a storefront actually asks.
    multi_row = [
        ["Colgate Total Toothpaste 150g", "Colgate", "Oral Care"],
        ["Colgate Mouthwash 500ml", "Colgate", "Oral Care"],
        ["Colgate Toothbrush Soft", "Colgate", "Accessory"],
        ["Oral B Toothbrush Medium", "Oral B", "Accessory"],
    ]
    with Index.build(
        lib,
        [("name", 3, 0.4), ("brand", 1, 0.6), ("category", 0, 0.6)],
        multi_row,
        facet=[1, 2],
    ) as mx:
        check(mx.facet_slot_count == 2, "two facet slots")
        check(mx.facet_count(1) == 2, "two distinct categories in slot 1")
        q = "Colgate Toothbrush"
        both = mx.search_facet_all(q, [(0, "Colgate"), (1, "Accessory")])
        check([h.doc for h in both] == [2], "the conjunction keeps only the Colgate accessory")
        # Each operand alone is strictly larger, which is what makes the conjunction meaningful.
        check(len(mx.search_facet(q, "Colgate", slot=0)) == 3, "brand alone is broader")
        check(len(mx.search_facet(q, "Accessory", slot=1)) == 2, "category alone is broader")
        check(
            mx.search_facet_all(q, [(0, "Oral B"), (1, "Oral Care")]) == [],
            "an unsatisfiable conjunction is empty, not everything",
        )
        check(
            mx.facet_tally(q, slot=1) == [("Accessory", 2), ("Oral Care", 2)],
            "per-slot tally counts every match",
        )

        # The rest of the filter bar. Both directions of the unknown-value rule are checked, because
        # reversing them turns a filter into a corpus dump that still looks like a working search.
        doc = lambda h: [x.doc for x in h]
        # A clause naming EVERY brand must reproduce the unfiltered ranking exactly -- same rows,
        # same order. A filter that reordered results would pass a set-equality check and fail here.
        check(
            doc(mx.search_clause(q, [Clause.any(0, ["Colgate", "Oral B"])])) == doc(mx.search(q)),
            "OR over every brand widens back to the unfiltered ranking, order included",
        )
        check(
            doc(mx.search_clause(q, [Clause.any(0, ["Colgate"]), Clause.any(1, ["Accessory"])]))
            == [2],
            "AND across clauses narrows to the Colgate accessory",
        )
        check(
            doc(mx.search_clause(q, [Clause.none(0, ["Colgate"])])) == [3],
            "NOT excludes the whole brand",
        )
        check(
            mx.search_clause(q, [Clause.any(0, ["Nestle"])]) == [],
            "an all-unknown INCLUDE matches nothing -- nobody can satisfy it",
        )
        check(
            len(mx.search_clause(q, [Clause.none(0, ["Nestle"])])) == 4,
            "an all-unknown EXCLUDE removes nothing -- there is nothing to remove",
        )
        try:
            mx.search_clause(q, [Clause.any(0, ["Colgate|Oral B"])])
            check(False, "a value containing '|' is refused rather than silently split")
        except ValueError:
            check(True, "a value containing '|' is refused rather than silently split")

        # Paging partitions the ranking: no gaps, no repeats, and past the end is empty.
        whole = doc(mx.search_page(q, 0, 4))
        paged = doc(mx.search_page(q, 0, 2)) + doc(mx.search_page(q, 2, 2))
        check(paged == whole, "two pages of two equal one request for four")
        check(len(set(paged)) == len(paged), "no document appears on two pages")
        check(mx.search_page(q, 1000, 4) == [], "past the end is empty rather than wrapped")

    # Numeric ranges: the other half of a filter bar.
    size_row = [
        ["Colgate Toothpaste Small", "Colgate", "50"],
        ["Colgate Toothpaste Medium", "Colgate", "100"],
        ["Colgate Toothpaste Large", "Colgate", "150"],
        ["Colgate Toothpaste Family", "Colgate", "300"],
        ["Colgate Toothpaste Sample", "Colgate", "not a number"],
    ]
    with Index.build(
        lib,
        [("name", 3, 0.4), ("brand", 1, 0.6), ("size", 0, 0.6)],
        size_row,
        numeric=2,
    ) as nx:
        check(nx.numeric_slot_count == 1, "one numeric column")
        q = "Colgate Toothpaste"
        check(
            len(nx.search_range(q, 100.0, 300.0)) == 2,
            "a half-open range excludes the value on the upper bound",
        )
        check(
            len(nx.search_range(q, -1e308, 1e308)) == 4,
            "the unparseable row is in no range at all",
        )
        hist = nx.range_tally(q, [0.0, 100.0, 200.0, 400.0])
        check(hist == [1, 2, 1], "buckets partition without double counting the boundary")
        check(sum(hist) == 4, "the absent value is counted nowhere")
        asc = [h.doc for h in nx.search_sorted(q)]
        check(asc == [0, 1, 2, 3], "ascending sort is by value: 50,100,150,300")
        check(
            [h.doc for h in nx.search_sorted(q, ascending=False)] == asc[::-1],
            "descending is the reverse",
        )
        check(len(asc) == 4, "the row with no numeric value is excluded from the order")

    # Phrase queries. The corpus is built so a bag-of-words search CANNOT tell the cases apart:
    # every row contains both words, and only adjacency separates them.
    phrase_row = [
        ["Vanilla Ice Cream Tub", "Selecta"],
        ["Ice Crushed Cream Soda", "Selecta"],
        ["Cream Ice Bar", "Selecta"],
        ["Chocolate Ice", "Cream Co"],  # adjacent only ACROSS the field boundary
    ]
    pf = [("name", 3, 0.4), ("brand", 1, 0.6)]
    with Index.build(lib, pf, phrase_row, position=True) as px:
        check(len(px.search("Ice Cream")) == 4, "every row matches as a bag of words")
        check(
            [h.doc for h in px.search_phrase("Ice Cream")] == [0],
            "only the adjacent, in-order, same-field row matches the phrase",
        )
        check(
            2 in [h.doc for h in px.search_phrase("Cream Ice")],
            "the reversed phrase matches the reversed row",
        )
        check(
            3 not in [h.doc for h in px.search_phrase("Ice Cream")],
            "a phrase must not span a field boundary",
        )
        check(px.search_phrase("Ice Sorbet") == [], "an unknown word matches nothing, not everything")
        check(
            [h.doc for h in px.search_phrase("Vanilla")] == [h.doc for h in px.search("Vanilla")],
            "a one-word phrase agrees with a one-word search",
        )

    # Without positions the phrase is REFUSED, not silently answered as a term search.
    with Index.build(lib, pf, phrase_row) as nopos:
        check(nopos.search_phrase("Ice Cream") == [], "no positions, no phrase results")
        check(len(nopos.search("Ice Cream")) == 4, "its ordinary search is unaffected")

    try:
        Index.build(lib, [("name", 3, 0.4)], [], facet=99)
        check(False, "an out-of-range facet field is refused")
    except ValueError:
        check(True, "an out-of-range facet field is refused")

    # Highlighting: which words matched.
    with Index.build(lib, [("name", 3, 0.4)], [["Colgate Total Toothpaste 150g"]]) as hx:
        body = "Colgate Total Toothpaste 150g"
        check(hx.highlight("Colgate", body) == ["Colgate"], "highlight marks the matching word")
        check(hx.highlight("Colgte", body) == ["Colgate"], "a typo highlights the corrected word")
        check(
            hx.highlight("Colgate Toothpaste", body) == ["Colgate", "Toothpaste"],
            "multiple spans, in order",
        )
        check(hx.highlight("Safeguard", body) == [], "a term absent from this text marks nothing")
        # Non-ASCII: byte offsets must still slice whole characters.
        check(
            hx.highlight("Colgate", "caf\u00e9 Colgate") == ["Colgate"],
            "byte offsets stay correct with multi-byte text before the match",
        )

    # Incremental updates: add rows without rebuilding, from Python.
    seg_field = [("name", 3, 0.4), ("brand", 1, 0.6)]
    base = Index.build(lib, seg_field, [
        ["Colgate Total Toothpaste 150g", "Colgate"],
        ["Aquafresh Mini Toothpaste 50g", "Aquafresh"],
    ], facet=1)
    with Live(lib, base) as live:
        check(live.doc_count == 2, "the searcher starts with the base segment")
        live.push(Index.build(lib, seg_field, [
            ["Aquafresh Twin Toothpaste 100g", "Aquafresh"],
            ["Colgate Travel Toothpaste 25g", "Colgate"],
        ], facet=1))
        check(live.segment_count == 2, "a segment was appended without rebuilding")
        check(live.doc_count == 4, "ordinals continue across the append")
        check(len(live.search("Toothpaste")) == 4, "a query reaches every segment")
        check(
            live.search("Colgte Travel")[0].doc == 3,
            "a typo query finds a row added after the build, at its global ordinal",
        )
        check(
            sorted(live.facet_tally("Toothpaste")) == [("Aquafresh", 2), ("Colgate", 2)],
            "the tally merges by value across segments, not by interned id",
        )
        # The filter bar and paging ACROSS segments. Segment 0 interned Colgate first and
        # segment 1 Aquafresh first, so the same label carries a different id in each -- resolution
        # has to happen by string, inside each segment.
        doc = lambda h: [x.doc for x in h]
        check(
            len(live.search_clause("Toothpaste", [Clause.any(0, ["Colgate", "Aquafresh"])])) == 4,
            "OR within a clause reaches both segments",
        )
        check(
            sorted(doc(live.search_clause("Toothpaste", [Clause.none(0, ["Colgate"])]))) == [1, 2],
            "an exclude drops only the excluded brand, in every segment",
        )
        check(
            live.search_clause("Toothpaste", [Clause.any(0, ["Nestle"])]) == [],
            "an all-unknown INCLUDE matches nothing across segments",
        )
        check(
            len(live.search_clause("Toothpaste", [Clause.none(0, ["Nestle"])])) == 4,
            "an all-unknown EXCLUDE removes nothing across segments",
        )

        # The page boundary is GLOBAL, so a segment's 2nd-best hit can be the page's 1st. A naive
        # per-segment slice would drop or duplicate it; this is the assertion that catches that.
        whole = doc(live.search_page("Toothpaste", 0, 4))
        paged = doc(live.search_page("Toothpaste", 0, 2)) + doc(live.search_page("Toothpaste", 2, 2))
        check(paged == whole, "pages partition the MERGED ranking, no gaps or repeats")
        check(len(set(paged)) == len(paged), "no document appears on two pages")
        check(live.search_page("Toothpaste", 99, 2) == [], "past the end is empty")

        check(live.delete(3), "a row added after the build can be deleted")
        check(live.live_count == 3, "live_count drops after a delete")

    # A phrase across segments, verified inside the segment that owns the row.
    pfield = [("name", 3, 0.4)]
    live2 = Live(lib, Index.build(lib, pfield, [
        ["Vanilla Ice Cream Tub"],
        ["Ice Crushed Cream Soda"],
    ], position=True))
    with live2:
        live2.push(Index.build(lib, pfield, [
            ["Mango Ice Cream Bar"],
            ["Cream Ice Bar"],
        ], position=True))
        check(
            sorted(h.doc for h in live2.search_phrase("Ice Cream")) == [0, 2],
            "a phrase is found at global ordinals in both segments",
        )
        check(len(live2.search("Ice Cream")) == 4, "the bag-of-words search still sees all four")
        # A segment with no positions must contribute NOTHING rather than term matches, which
        # would be indistinguishable from phrase matches once merged.
        live2.push(Index.build(lib, pfield, [["Strawberry Ice Cream Cone"]]))
        check(
            sorted(h.doc for h in live2.search_phrase("Ice Cream")) == [0, 2],
            "only segments that CAN verify contribute to a phrase",
        )

    # ---- The image tier, from Python -------------------------------------------------------
    #
    # Mirrors `js/image-smoke.mjs` assertion for assertion, on the same six-image corpus and the
    # same probe, so a divergence between the WASM host and this native one shows up as one tier
    # failing a check the other passes. The embeddings are packed by `struct.pack` alone: if this
    # section passes, the fused image query is reachable from any language with `dlopen` and no
    # numerical library at all.
    image_check(lib, check)

    # Application keys: saying WHICH ROW, which is what a change stream needs.
    kf = [("sku", 0, 0.6), ("name", 3, 0.4)]
    with Index.build(lib, kf, [
        ["sku-1", "Colgate Total Toothpaste 150g"],
        ["sku-2", "Aquafresh Mini Toothpaste 50g"],
        ["", "Unkeyed Toothpaste"],
    ], key=0) as kx:
        check(kx.keyed_count == 2, "a blank key field is NO key, so only two rows are addressable")
        check(kx.doc_of_key("sku-1") == 0, "a key resolves to its document")
        check(kx.doc_of_key("sku-404") is None, "an unknown key resolves to nothing")
        check(kx.key_of(1) == "sku-2", "and the key reads back out")
        check(kx.key_of(2) is None, "the unkeyed row reports no key")

    live3 = Live(lib, Index.build(lib, kf, [
        ["sku-1", "Colgate Total Toothpaste 150g"],
        ["sku-2", "Aquafresh Mini Toothpaste 50g"],
    ], key=0))
    with live3:
        check(live3.has_key, "the collection is fully keyed")
        # An UPDATE: append the new version; push retires the old one.
        live3.push(Index.build(lib, kf, [
            ["sku-1", "Colgate Total Charcoal Toothpaste 200g"],
            ["sku-3", "Oral B Toothpaste Pro 120g"],
        ], key=0))
        check(live3.live_count == 3, "the superseded row was retired, not accumulated")
        check(live3.doc_of_key("sku-1") == 2, "the LIVE sku-1 is the new version")
        check(live3.key_of(2) == "sku-1", "a global ordinal reads back its key")
        check(
            len([h for h in live3.search("Charcoal")]) == 1,
            "the updated row is findable by its new text",
        )
        # A DELETE, expressed the only way an application can.
        check(live3.delete_key("sku-2"), "a delete by key retires the row")
        check(not live3.delete_key("sku-2"), "replaying a delete is a no-op, not an error")
        check(not live3.delete_key("sku-404"), "deleting a key that never existed reports False")
        check(live3.doc_of_key("sku-2") is None, "and the row is gone")
        check(live3.live_count == 2, "two live rows remain")

    check(Index.build(lib, [("name", 3, 0.4)], []).doc_count == 0, "an empty corpus builds and is empty")

    try:
        Index.open(lib, b"GARBAGE!")
        check(False, "garbage bytes are refused rather than crashing")
    except ValueError:
        check(True, "garbage bytes are refused rather than crashing")

    print("OVERALL: PASS" if failed == 0 else f"OVERALL: FAIL ({failed})")
    return 0 if failed == 0 else 1


# ---- Real-corpus bench ----------------------------------------------------------------------
# The self-test above proves the binding works. This proves the binding is not where the speed
# goes: it times the SAME engine from Python that `index-bench` times from Rust, on real rows, so
# the FFI overhead per query is visible rather than assumed.


def bench(multiplier: int = 1) -> int:
    import time

    lib = _bind(_load())
    fixture = ROOT / "bench" / "fixture" / "blead-lead.tsv"
    if not fixture.exists():
        print(f"missing fixture: {fixture}")
        return 1

    line = fixture.read_text(encoding="utf-8", errors="replace").splitlines()
    header, body = line[0].split("\t"), line[1:]
    row = [r.split("\t") for r in body if r.count("\t") == len(header) - 1]

    label = "real"
    if multiplier > 1:
        # Recombined, and labelled as such: suffixing the name keeps terms distinct so the
        # dictionary grows the way a larger real corpus would, instead of measuring a corpus of
        # duplicates that every posting list collapses.
        base, row = row, []
        for i in range(multiplier):
            for r in base:
                row.append([f"{r[0]} {i}" if i else r[0], *r[1:]])
        label = "recombined"

    field = [("name", 3.0, 0.4), ("industry", 1.0, 0.6), ("city", 1.0, 0.6)]

    t0 = time.perf_counter()
    idx = Index.build(lib, field, row)
    build_ms = (time.perf_counter() - t0) * 1e3

    query = [r[0] for r in row[:: max(1, len(row) // 2000)]][:2000]
    typo = [q[:-1] + "x" if len(q) > 4 else q for q in query]

    def timed(q_list: list[str]) -> tuple[float, float]:
        us = []
        for q in q_list:
            t = time.perf_counter()
            idx.search(q, 10)
            us.append((time.perf_counter() - t) * 1e6)
        us.sort()
        return us[len(us) // 2], us[int(len(us) * 0.99)]

    exact_p50, exact_p99 = timed(query)
    typo_p50, typo_p99 = timed(typo)

    print("index :: real-corpus bench, driven entirely from Python via ctypes")
    print(f"  corpus       {len(row):,} rows ({label}), {idx.term_count:,} terms")
    print(f"  build        {build_ms:,.0f} ms")
    print(f"  exact  p50   {exact_p50:,.0f} us     p99 {exact_p99:,.0f} us")
    print(f"  typo   p50   {typo_p50:,.0f} us     p99 {typo_p99:,.0f} us")
    print(f"  queries      {len(query):,} real business names, k=10")
    print()
    print("  These include the full ctypes round trip -- argument marshalling, the call, and")
    print("  unpacking every hit into a Python object. No upload, no server, no Rust build step")
    print("  in the host: `cargo build -p index-wasm --release` once, then `import ctypes`.")
    idx.close()
    return 0


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--bench":
        sys.exit(bench(int(sys.argv[2]) if len(sys.argv) > 2 else 1))
    sys.exit(main())
