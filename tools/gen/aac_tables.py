#!/usr/bin/env python3
"""Generate `modules/app/codec/audio/aac/tables.rs` for the clean-room AAC-LC
decoder, from the decoder specification and from formulas.

WHY THIS EXISTS

The decoder is a clean-room implementation (`.context/clean_room/aac/`): its
author read no decoder source, and every constant in the decoder must be
traceable to a specification revision. This script is that trace for the
tables. It reads the data tables of the specification —
the Huffman codebooks, the scalefactor-band boundaries, the sampling-rate and
TNS limit tables — and emits them as Rust, and it computes from the spec's
formulas the tables the spec says an implementation must compute for itself:
the Kaiser–Bessel-derived windows, the |q|^(4/3) dequantisation table, the
2^(x/4) gain table, and the quarter-wave cosine the filterbank's twiddles are
read from.

Before writing anything it re-checks what the spec claims about its own tables
(every codebook prefix-free with Kraft sum exactly 1; every band width a
multiple of 4 ending at 1024/128) and checks the fast IMDCT decomposition the
decoder uses against the spec's direct definition, so a wrong derivation
cannot reach the tree.

It also writes `tools/gen/aac_codebooks.txt`, a plain copy of the codebook
rows (index, length, codeword, values), which the harness's provenance test
reads: `.context/` is private and absent from a clean checkout, so the test
checks the generated tries against that committed copy instead.

USAGE
    python3 tools/gen/aac_tables.py            # check, print a summary
    python3 tools/gen/aac_tables.py --write    # also write tables.rs + codebooks.txt

Citations in the output are to `[spec r01 §x.y]` / `[spec r01 Tn]`, the
specification revision this script was run against.
"""
import math
import re
import sys
from fractions import Fraction
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / ".context/clean_room/aac/spec/aac_lc_r01_released.md"
OUT_RS = ROOT / "modules/app/codec/audio/aac/tables.rs"
OUT_TXT = ROOT / "tools/gen/aac_codebooks.txt"
SPEC_REV = "r01"

# ── spec parsing ────────────────────────────────────────────────────────────

def fenced_blocks_after(text, heading):
    """Every ``` block between `heading` and the next heading of equal or
    higher level, each as a list of non-empty lines."""
    start = text.index(heading)
    level = heading.split(" ")[0]
    rest = text[start + len(heading):]
    m = re.search(r"^#{1,%d} " % len(level), rest, re.M)
    section = rest[: m.start()] if m else rest
    blocks = re.findall(r"```\n(.*?)```", section, re.S)
    return [[l for l in b.split("\n") if l.strip()] for b in blocks]


def parse_named_tables(text, heading, prefix):
    """`Table Lxxxxx: N bands ...` paragraphs followed by one fenced block of
    `band start end width` rows. Returns {name: [starts..., final]}."""
    start = text.index(heading)
    rest = text[start + len(heading):]
    m = re.search(r"^## ", rest, re.M)
    section = rest[: m.start()]
    out = {}
    for name, block in re.findall(r"Table (%s\d+): .*?```\n(.*?)```" % prefix, section, re.S):
        rows = [l.split() for l in block.split("\n") if l.strip() and not l.startswith("band")]
        starts = [int(r[1]) for r in rows]
        ends = [int(r[2]) for r in rows]
        for i in range(1, len(rows)):
            assert starts[i] == ends[i - 1], (name, i)
        assert all((e - s) % 4 == 0 for s, e in zip(starts, ends)), name
        out[name] = starts + [ends[-1]]
    return out


def parse_codebook(text, table_no):
    heading = "#### T%d — " % table_no
    blocks = fenced_blocks_after(text, heading)
    rows = []
    for line in blocks[0][1:]:
        f = line.split()
        index, length, cbin, chex = int(f[0]), int(f[1]), f[2], int(f[3], 16)
        values = [int(v) for v in f[4:]]
        assert len(cbin) == length and int(cbin, 2) == chex, line
        rows.append((index, length, chex, values))
    assert [r[0] for r in rows] == list(range(len(rows))), "index order"
    return rows


def check_codebook(name, rows, expect_entries):
    assert len(rows) == expect_entries, (name, len(rows))
    kraft = sum(Fraction(1, 2 ** r[1]) for r in rows)
    assert kraft == 1, (name, kraft)
    codes = sorted(((r[2], r[1]) for r in rows), key=lambda c: c[1])
    for i, (ca, la) in enumerate(codes):
        for cb, lb in codes[i + 1:]:
            if lb >= la and (cb >> (lb - la)) == ca:
                raise AssertionError("%s: %#x/%d is a prefix of %#x/%d" % (name, ca, la, cb, lb))


def build_trie(rows):
    """Binary trie as a flat node list: node[n] = (child0, child1); a child
    value with bit 15 set is a leaf holding the row index."""
    nodes = [[None, None]]
    for index, length, code, _ in rows:
        n = 0
        for bit_pos in range(length - 1, -1, -1):
            bit = (code >> bit_pos) & 1
            if bit_pos == 0:
                assert nodes[n][bit] is None
                nodes[n][bit] = 0x8000 | index
            else:
                if nodes[n][bit] is None:
                    nodes.append([None, None])
                    nodes[n][bit] = len(nodes) - 1
                n = nodes[n][bit]
    for c0, c1 in nodes:
        assert c0 is not None and c1 is not None, "incomplete code"
    assert len(nodes) < 0x8000
    return nodes


def decode_with_trie(nodes, bits):
    n = 0
    for b in bits:
        v = nodes[n][b]
        if v & 0x8000:
            return v & 0x7FFF
        n = v
    raise AssertionError("ran out of bits")

# ── formula tables ───────────────────────────────────────────────────────────

def bessel_i0(x):
    """I0(x) = Σ ((x/2)^m / m!)^2  [spec r01 §8.2]."""
    term, total, m = 1.0, 1.0, 0
    while True:
        m += 1
        term *= (x / 2.0) / m
        contrib = term * term
        total += contrib
        if contrib < total * 1e-17:
            return total


def kbd_window(n_half, alpha):
    """Rising half of the KBD window, N = 2 * n_half  [spec r01 §8.2]."""
    quarter = n_half // 2
    kernel = [bessel_i0(math.pi * alpha * math.sqrt(max(0.0, 1.0 - ((j - quarter) / quarter) ** 2))) / bessel_i0(math.pi * alpha)
              for j in range(n_half + 1)]
    total = sum(kernel)
    out, acc = [], 0.0
    for n in range(n_half):
        acc += kernel[n]
        out.append(math.sqrt(acc / total))
    return out


def check_imdct_derivation():
    """The decoder computes the IMDCT [spec r01 §8.1] as a DCT-IV (via an
    N/8-point complex FFT with twiddles exp(-iπ(j+1/8)/M)) followed by an
    unfold with signs. Check that decomposition against the direct definition
    for both frame sizes on random input."""
    import cmath
    import random
    random.seed(20260925)

    def direct(X, N):
        n0 = (N / 2 + 1) / 2
        return [(2 / N) * sum(X[k] * math.cos(2 * math.pi / N * (n + n0) * (k + 0.5)) for k in range(N // 2)) for n in range(N)]

    def fft(a):
        n = len(a)
        if n == 1:
            return a[:]
        e, o = fft(a[0::2]), fft(a[1::2])
        return [e[k] + cmath.exp(-2j * math.pi * k / n) * o[k] for k in range(n // 2)] + \
               [e[k] - cmath.exp(-2j * math.pi * k / n) * o[k] for k in range(n // 2)]

    def dct4(X, M):
        Q = M // 2
        v = [(X[2 * j] + 1j * X[M - 1 - 2 * j]) * cmath.exp(-1j * math.pi * (j + 0.125) / M) for j in range(Q)]
        w = fft(v)
        w = [w[j] * cmath.exp(-1j * math.pi * (j + 0.125) / M) for j in range(Q)]
        y = [0.0] * M
        for j in range(Q):
            y[2 * j] = w[j].real
            y[M - 1 - 2 * j] = -w[j].imag
        return y

    def fast(X, N):
        M = N // 2
        y = dct4(X, M)
        x = [0.0] * N
        for n in range(N):
            if n < M // 2:
                x[n] = (2 / N) * y[n + M // 2]
            elif n < 3 * M // 2:
                x[n] = -(2 / N) * y[3 * M // 2 - 1 - n]
            else:
                x[n] = -(2 / N) * y[n - 3 * M // 2]
        return x

    for N in (256, 2048):
        X = [random.uniform(-1, 1) for _ in range(N // 2)]
        a, b = direct(X, N), fast(X, N)
        err = max(abs(p - q) for p, q in zip(a, b))
        assert err < 1e-9, (N, err)

# ── emission ────────────────────────────────────────────────────────────────

def f32(v):
    """The shortest decimal literal that rounds to the intended f32 exactly
    (clippy's `excessive_precision` refuses digits an f32 cannot hold)."""
    import struct
    bits = struct.pack("f", v)
    # Search from the f32 value itself, so the shortest literal is the one
    # Rust prints for it (clippy compares against exactly that).
    r = struct.unpack("f", bits)[0]
    for digits in range(1, 10):
        s = "%.*g" % (digits, r)
        if struct.pack("f", float(s)) == bits:
            break
    if "e" not in s and "." not in s:
        s += ".0"
    return s


def rust_array(name, ty, values, per_line, cite, doc, attrs=()):
    lines = ["/// %s" % doc, "/// %s" % cite, *attrs,
             "pub static %s: [%s; %d] = [" % (name, ty, len(values))]
    for i in range(0, len(values), per_line):
        lines.append("    " + ", ".join(values[i:i + per_line]) + ",")
    lines.append("];")
    return "\n".join(lines) + "\n"


def main():
    write = "--write" in sys.argv
    text = SPEC.read_text()

    # T1: sampling rates and which band tables each index uses [spec r01 §2.3].
    t1 = [l.split() for l in fenced_blocks_after(text, "### §2.3 Sampling-frequency index — T1")[0][1:] if l.split()[0].isdigit() and len(l.split()) == 6]
    assert len(t1) == 13
    rates = [int(r[1]) for r in t1]
    long_names = [r[2] for r in t1]
    short_names = [r[3] for r in t1]
    n_long = [int(r[4]) for r in t1]
    n_short = [int(r[5]) for r in t1]

    longs = parse_named_tables(text, "### §5.1 Long windows (1024 coefficients) — T3", "L")
    shorts = parse_named_tables(text, "### §5.2 Short windows (128 coefficients) — T4", "S")
    for i in range(13):
        assert len(longs[long_names[i]]) - 1 == n_long[i], i
        assert len(shorts[short_names[i]]) - 1 == n_short[i], i
        assert longs[long_names[i]][-1] == 1024 and shorts[short_names[i]][-1] == 128
    long_order = sorted(longs, key=lambda n: -int(n[1:]))
    short_order = sorted(shorts, key=lambda n: -int(n[1:]))

    # T17: TNS band limits per sampling index [spec r01 §7.7].
    t17 = [l.split() for l in fenced_blocks_after(text, "### §7.7 TNS limits — T17")[0][1:]]
    assert len(t17) == 13
    tns_long = [int(r[2]) for r in t17]
    tns_short = [int(r[3]) for r in t17]

    # T18: TNS reflection coefficients [spec r01 §7.6, §7.8], indexed by the
    # signed code value masked to the nominal resolution.
    def tns_coefs(res):
        qp = (2 ** (res - 1) - 0.5) / (math.pi / 2)
        qn = (2 ** (res - 1) + 0.5) / (math.pi / 2)
        out = []
        for code in range(2 ** res):
            s = code if code < 2 ** (res - 1) else code - 2 ** res
            out.append(math.sin(s / qp) if s >= 0 else math.sin(s / qn))
        return out
    t18_blocks = fenced_blocks_after(text, "### §7.8 TNS coefficient table — T18")
    spec_t18_3 = [float(l.split()[2]) for l in t18_blocks[0][1:]]
    spec_t18_4 = [float(l.split()[2]) for l in t18_blocks[1][1:]]
    tns3, tns4 = tns_coefs(3), tns_coefs(4)
    assert all(abs(a - b) < 1e-9 for a, b in zip(tns3, spec_t18_3))
    assert all(abs(a - b) < 1e-9 for a, b in zip(tns4, spec_t18_4))

    # Codebooks T5..T16 [spec r01 §6].
    books = []
    expected = [121, 81, 81, 81, 81, 81, 81, 64, 64, 169, 169, 289]
    dims = [1, 4, 4, 4, 4, 2, 2, 2, 2, 2, 2, 2]
    for t, (n_entries, dim) in enumerate(zip(expected, dims), start=5):
        rows = parse_codebook(text, t)
        check_codebook("T%d" % t, rows, n_entries)
        for index, _, _, values in rows:
            assert len(values) == dim, (t, index)
        trie = build_trie(rows)
        for index, length, code, _ in rows:
            bits = [(code >> (length - 1 - i)) & 1 for i in range(length)]
            assert decode_with_trie(trie, bits) == index
        books.append((t, dim, rows, trie))
    assert all(r[3][0] == r[0] - 60 for r in books[0][2])

    check_imdct_derivation()

    kbd_long = kbd_window(1024, 4)
    kbd_short = kbd_window(128, 6)
    for w, half in ((kbd_long, 1024), (kbd_short, 128)):
        # Princen–Bradley: W[n]^2 + W[n + N/2]^2 = 1 with W symmetric.
        assert all(abs(w[n] ** 2 + w[half - 1 - n] ** 2 - 1) < 1e-12 for n in range(half))
    cos_q = [math.cos(2 * math.pi * i / 16384) for i in range(4097)]
    iq = [i ** (4 / 3) for i in range(8192)]
    pow2 = [2 ** ((i - 128) / 4) for i in range(289)]

    print("spec %s: T1 ok, %d long tables, %d short tables, 12 codebooks ok, IMDCT derivation ok" % (SPEC_REV, len(longs), len(shorts)))
    if not write:
        return

    o = []
    o.append("// Tables of the clean-room AAC-LC decoder. GENERATED by\n"
             "// `tools/gen/aac_tables.py` from the clean-room decoder specification\n"
             "// (`.context/clean_room/aac/spec/aac_lc_%s_released.md`) and from the\n"
             "// formulas it states; do not edit. Every table cites the section or table\n"
             "// of that revision it comes from. The codebook rows are also kept as\n"
             "// `tools/gen/aac_codebooks.txt`, which the provenance test checks these\n"
             "// tries against. Plain comments, not doc comments, so the file can be\n"
             "// `include!`d as well as declared as a module.\n\n" % SPEC_REV)
    o.append("/// Sampling rate per 4-bit sampling-frequency index. [spec %s T1]\n" % SPEC_REV)
    o.append("pub static SAMPLE_RATES: [u32; 13] = [%s];\n\n" % ", ".join(str(r) for r in rates))
    o.append("/// Long-window band table per sampling index (an index into `BANDS_LONG`). [spec %s T1]\n" % SPEC_REV)
    o.append("pub static LONG_TABLE_OF: [u8; 13] = [%s];\n" % ", ".join(str(long_order.index(n)) for n in long_names))
    o.append("/// Short-window band table per sampling index (an index into `BANDS_SHORT`). [spec %s T1]\n" % SPEC_REV)
    o.append("pub static SHORT_TABLE_OF: [u8; 13] = [%s];\n\n" % ", ".join(str(short_order.index(n)) for n in short_names))
    o.append("/// Band start indices for long windows, one table per rate family, each\n"
             "/// ending with the final boundary 1024; the band count is the length minus one.\n"
             "/// Order: %s. [spec %s T3]\n" % (", ".join(long_order), SPEC_REV))
    o.append("pub static BANDS_LONG: [&[u16]; %d] = [\n" % len(long_order))
    for n in long_order:
        o.append("    &[%s],\n" % ", ".join(str(v) for v in longs[n]))
    o.append("];\n")
    o.append("/// Band start indices for short windows, ending with 128.\n/// Order: %s. [spec %s T4]\n" % (", ".join(short_order), SPEC_REV))
    o.append("pub static BANDS_SHORT: [&[u16]; %d] = [\n" % len(short_order))
    for n in short_order:
        o.append("    &[%s],\n" % ", ".join(str(v) for v in shorts[n]))
    o.append("];\n\n")
    o.append("/// TNS: highest band (exclusive) a filter may reach, long windows. [spec %s T17]\n" % SPEC_REV)
    o.append("pub static TNS_MAX_BAND_LONG: [u8; 13] = [%s];\n" % ", ".join(str(v) for v in tns_long))
    o.append("/// TNS: highest band (exclusive) a filter may reach, short windows. [spec %s T17]\n" % SPEC_REV)
    o.append("pub static TNS_MAX_BAND_SHORT: [u8; 13] = [%s];\n" % ", ".join(str(v) for v in tns_short))
    o.append(rust_array("TNS_COEF_3", "f32", [f32(v) for v in tns3], 4, "[spec %s §7.6, T18]" % SPEC_REV,
                        "TNS reflection coefficients at 3-bit resolution, indexed by the code's signed value masked to 3 bits (a compressed 2-bit code masks the same way)."))
    o.append(rust_array("TNS_COEF_4", "f32", [f32(v) for v in tns4], 4, "[spec %s §7.6, T18]" % SPEC_REV,
                        "TNS reflection coefficients at 4-bit resolution, indexed by the code's signed value masked to 4 bits."))
    o.append("\n")
    o.append("/// One Huffman codebook as a binary trie. `nodes[n][bit]` is the next node, or,\n"
             "/// with bit 15 set, a leaf carrying the row index. Every trie is complete\n"
             "/// (Kraft sum 1), so any bit string of sufficient length reaches a leaf.\n"
             "/// `values` holds `dim` values per row, row-major.\n")
    o.append("pub struct Codebook {\n    pub nodes: &'static [[u16; 2]],\n    pub dim: u8,\n    pub values: &'static [i8],\n}\n\n")
    for t, dim, rows, trie in books:
        tag = "SF" if t == 5 else str(t - 5)
        node_lits = ["[%d, %d]" % (c0, c1) for c0, c1 in trie]
        o.append(rust_array("HCB_%s_NODES" % tag, "[u16; 2]", node_lits, 6, "[spec %s T%d]" % (SPEC_REV, t),
                            "Trie of %s (%d rows, %d nodes)." % ("the scalefactor codebook" if t == 5 else "spectral codebook %d" % (t - 5), len(rows), len(trie))))
        vals = [str(v) for r in rows for v in r[3]]
        o.append(rust_array("HCB_%s_VALUES" % tag, "i8", vals, 16 if dim == 4 else 16, "[spec %s T%d]" % (SPEC_REV, t),
                            "Decoded values of %s, %d per row." % ("the scalefactor codebook (index - 60)" if t == 5 else "spectral codebook %d" % (t - 5), dim)))
        o.append("pub static HCB_%s: Codebook = Codebook { nodes: &HCB_%s_NODES, dim: %d, values: &HCB_%s_VALUES };\n\n" % (tag, tag, dim, tag))
    o.append("/// Spectral codebooks 1..=11 by number (index 0 is a placeholder: codebook 0 codes nothing). [spec %s §6.1]\n" % SPEC_REV)
    o.append("pub static HCB_SPECTRAL: [&Codebook; 12] = [&HCB_1, %s];\n" % ", ".join("&HCB_%d" % i for i in range(1, 12)))
    o.append("/// Whether spectral codebook `cb` carries signs in its values (else sign bits follow). [spec %s §6.1]\n" % SPEC_REV)
    o.append("pub static HCB_SIGNED: [bool; 12] = [false, true, true, false, false, true, true, false, false, false, false, false];\n\n")
    o.append(rust_array("KBD_LONG", "f32", [f32(v) for v in kbd_long], 6, "[spec %s §8.2]" % SPEC_REV,
                        "Rising half of the Kaiser–Bessel-derived window, N = 2048, alpha = 4."))
    o.append(rust_array("KBD_SHORT", "f32", [f32(v) for v in kbd_short], 6, "[spec %s §8.2]" % SPEC_REV,
                        "Rising half of the Kaiser–Bessel-derived window, N = 256, alpha = 6."))
    o.append(rust_array("COS_QUARTER", "f32", [f32(v) for v in cos_q], 6, "[spec %s §8.1, §8.2]" % SPEC_REV,
                        "cos(2πi/16384) for i = 0..=4096: a quarter wave on the grid every filterbank twiddle and sine-window sample lies on."))
    o.append(rust_array("IQ_TABLE", "f32", [f32(v) for v in iq], 6, "[spec %s §7.1]" % SPEC_REV,
                        "|q|^(4/3) for q = 0..8191, the dequantisation magnitude table."))
    o.append(rust_array("POW2_QUARTER", "f32", [f32(v) for v in pow2], 6, "[spec %s §7.1, §7.4, §7.5]" % SPEC_REV,
                        "2^((i - 128)/4) for i = 0..288, covering scalefactor gains (x = sf - 100) and the ±120 intensity/noise range.",
                        attrs=['#[expect(clippy::approx_constant, reason = "2^(2/4) is sqrt(2); the table is one formula, not a list of constants")]']))
    OUT_RS.write_text("".join(o))
    # `modules/**` is fmt-checked directly, so the output must already be
    # formatted for whoever regenerates next.
    subprocess.run(["rustfmt", "--edition", "2021", str(OUT_RS)], check=True)

    lines = ["# Codebook rows of the clean-room AAC-LC decoder specification %s (T5..T16):" % SPEC_REV,
             "# table dim index length codeword_hex values...  — facts of the standard, emitted by tools/gen/aac_tables.py"]
    for t, dim, rows, _ in books:
        for index, length, code, values in rows:
            lines.append("T%d %d %d %d %#x %s" % (t, dim, index, length, code, " ".join(str(v) for v in values)))
    OUT_TXT.write_text("\n".join(lines) + "\n")
    print("wrote", OUT_RS.relative_to(ROOT), "and", OUT_TXT.relative_to(ROOT))


if __name__ == "__main__":
    main()
