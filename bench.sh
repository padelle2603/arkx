#!/bin/bash
# bench.sh — benchmark riproducibile per arkx su QUALSIASI macchina.
# Misura tempo + rapporto su un corpus sintetico misto (file piccoli +
# un file grande), per formato x livello. Uso:
#   ./bench.sh [binario]   # default: ./target/release/arkx
set -u
BIN="${1:-./target/release/arkx}"
WORK="${ARKX_BENCH_DIR:-/tmp/arkx-bench}"
mkdir -p "$WORK/corpus/small" "$WORK/out"

echo "== corpus =="
if [ ! -f "$WORK/corpus/big.bin" ]; then
    python3 - "$WORK/corpus" << 'EOF'
import os, random
base = os.path.join(__import__('sys').argv[1])
random.seed(42)
# 300 file di testo comprimibili (16KB l'uno)
for i in range(300):
    with open(os.path.join(base, 'small', f'f{i:04d}.txt'), 'w') as f:
        f.write((f"riga di benchmark numero {i} " * 64 + "\n") * 8)
# 64MB pseudo-casuali (poco comprimibili)
rnd = random.Random(7)
with open(os.path.join(base, 'big.bin'), 'wb') as f:
    for _ in range(64):
        f.write(bytes(rnd.getrandbits(8) for _ in range(1024 * 1024)))
print("corpus created")
EOF
fi
SRC_SIZE=$(du -sb "$WORK/corpus" | cut -f1)
echo "input: $SRC_SIZE byte ($(numfmt --to=iec-i "$SRC_SIZE"))  threads: $("$BIN" --version)"

now_ns() { date +%s%N; }
run_case() { # fmt level  (fmt = zip | tar.gz | tar.zst | 7z)
    local fmt="$1" level="$2"
    local dest="$WORK/out/bench-l$level.$fmt"
    rm -f "$dest"
    local t0 t1 ms size ratio extra=()
    if [ -n "${ARKX_THREADS:-}" ]; then extra=(--threads "$ARKX_THREADS"); fi
    t0=$(now_ns)
    "$BIN" a "$dest" "$WORK/corpus" -l "$level" "${extra[@]}" > /dev/null 2>&1
    t1=$(now_ns)
    ms=$(( (t1 - t0) / 1000000 ))
    if [ ! -f "$dest" ]; then printf "%-10s l%-2s  FAIL\n" "$fmt" "$level"; return; fi
    size=$(stat -c%s "$dest")
    ratio=$(python3 -c "print(f'{$size/$SRC_SIZE*100:.1f}%')")
    printf "%-10s l%-2s  %8s ms  %10s byte  ratio %s\n" "$fmt" "$level" "$ms" "$size" "$ratio"
}

echo "== creazione =="
run_case "zip" 1; run_case "zip" 6
run_case "tar.gz" 1; run_case "tar.gz" 6; run_case "tar.gz" 9
run_case "tar.zst" 1; run_case "tar.zst" 6; run_case "tar.zst" 9
run_case "7z" 1; run_case "7z" 6

echo "== estrazione (livello 6) =="
for a in "$WORK"/out/bench-l6.*; do
    [ -f "$a" ] || continue
    rm -rf "$WORK/x"; mkdir -p "$WORK/x"
    t0=$(now_ns)
    "$BIN" x "$a" "$WORK/x" > /dev/null 2>&1
    t1=$(now_ns)
    printf "%-22s  %8s ms\n" "$(basename "$a")" "$(( (t1 - t0) / 1000000 ))"
done
echo "== fine (corpus in $WORK, output in $WORK/out) =="
