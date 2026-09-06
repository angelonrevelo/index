// Range reads over an OPFS-resident index, in a worker.
//
// `createSyncAccessHandle()` is worker-only in every engine that ships it, and it is the only OPFS
// API that can read a SLICE of a file without materialising the whole thing. That is what turns the
// section table in `format.rs` from a nicety into the browser's cold-tier story:
//
//   read MAGIC.len() + 288 bytes  ->  every section's (offset, len)
//   read one posting span         ->  answer a term query
//
// without the other 900 MB ever entering the tab. `format::posting_span` and
// `a_single_posting_list_is_range_readable` exist for exactly this consumer; until now nothing
// actually exercised them from a browser.
//
// The worker speaks a tiny message protocol rather than exposing handles, because a sync access
// handle is exclusive: two holders on one file is an error, so ownership stays here.

/** Bytes of magic + section table. Mirrors `MAGIC.len() + TABLE_BYTE` in `format.rs`. */
const HEAD_BYTE = 8 + 18 * 16;

let handle = null;
let name = null;

async function openHandle(file) {
  if (handle && name === file) {
    return handle;
  }
  if (handle) {
    handle.close();
  }
  const root = await navigator.storage.getDirectory();
  const d = await root.getDirectoryHandle('index-tier', { create: true });
  const h = await d.getFileHandle(file);
  handle = await h.createSyncAccessHandle();
  name = file;
  return handle;
}

/** Read `len` bytes at `at`, without reading anything else. */
function readAt(h, at, len) {
  const buf = new Uint8Array(len);
  const n = h.read(buf, { at });
  return n === len ? buf : buf.subarray(0, n);
}

/** Decode the section table the same way `read_section_table` does: 18 spans of two little-endian u64. */
function sections(head) {
  if (String.fromCharCode(...head.subarray(0, 8)) !== 'IDXTEXT9') {
    throw new Error(`bad magic: ${String.fromCharCode(...head.subarray(0, 8))}`);
  }
  const dv = new DataView(head.buffer, head.byteOffset + 8);
  const name = [
    'meta', 'schema', 'alias', 'dict', 'posting_offset', 'posting', 'doc_len', 'prior',
    'first_term', 'deleted', 'expansion', 'facet_label', 'facet_id', 'numeric_field',
    'numeric_value', 'position_at', 'position', 'doc_key',
  ];
  const out = {};
  for (let i = 0; i < name.length; i++) {
    // u64 offsets; Number is exact to 2^53, far past any index this format can address.
    out[name[i]] = {
      offset: Number(dv.getBigUint64(i * 16, true)),
      len: Number(dv.getBigUint64(i * 16 + 8, true)),
    };
  }
  return out;
}

self.onmessage = async (e) => {
  const { id, op, file, at, len } = e.data;
  try {
    const h = await openHandle(file);
    let result;
    switch (op) {
      case 'head': {
        // The claim: learn the whole layout from 296 bytes, whatever the file weighs.
        const head = readAt(h, 0, HEAD_BYTE);
        result = { size: h.getSize(), headBytes: head.length, section: sections(head) };
        break;
      }
      case 'range': {
        const buf = readAt(h, at, len);
        result = { read: buf.length, checksum: buf.reduce((a, b) => (a + b) & 0xffffffff, 0) };
        break;
      }
      case 'all': {
        const buf = readAt(h, 0, h.getSize());
        self.postMessage({ id, ok: true, bytes: buf }, [buf.buffer]);
        return;
      }
      case 'close': {
        h.close();
        handle = null;
        name = null;
        result = { closed: true };
        break;
      }
      default:
        throw new Error(`unknown op ${op}`);
    }
    self.postMessage({ id, ok: true, result });
  } catch (err) {
    self.postMessage({ id, ok: false, error: String(err) });
  }
};
