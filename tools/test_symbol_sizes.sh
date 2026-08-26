#!/usr/bin/env bash
# test_symbol_sizes.sh — #390: prove tools/check_symbol_sizes.py can fail, and that it fails for
# the right reasons and passes for the right reasons.
#
# The fixture is a REAL SLICE of `readelf -SW` / `readelf -sW` output from the canonical C3 ELF
# (espnow,cast,io), not a hand-invented one. That matters here for the same reason it mattered in
# test_config_markers.sh: a hand-written stand-in drifts from the thing being guarded, and the
# thing being guarded is a specific tool's output format. Every symbol row below was copied from a
# real build — including the awkward ones (`.L_MergedGlobals`, `.Lswitch.table`, `.Lanon`, six
# different crate disambiguators, and a 2560 B .rodata static that must NOT be tracked).
#
# DIVISION OF LABOUR, because neither half is sufficient alone:
#   * THIS suite proves the PARSE + NORMALISE + COMPARE logic, offline, with no cargo and no ELF.
#   * The gate.sh fw arm proves the REAL `readelf` invocation still parses what this fixture
#     claims it parses. A readelf output-format change would keep this suite green and turn that
#     arm red — which is the correct split, since only one of the two can notice it.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$HERE/check_symbol_sizes.py"
[ -f "$CHECK" ] || { echo "missing $CHECK" >&2; exit 2; }

pass=0; fail=0
ok(){ pass=$((pass+1)); echo "ok   - $1"; }
no(){ fail=$((fail+1)); echo "FAIL - $1"; }
eq(){ if [ "$2" = "$3" ]; then ok "$1 ($2)"; else no "$1: want [$2] got [$3]"; fi; }

# Never /tmp (JP directive: katana's /tmp is a 16 GB tmpfs).
TMPROOT="${TMPDIR:-/var/tmp}"; case "$TMPROOT" in /tmp|/tmp/*) TMPROOT=/var/tmp ;; esac
W="$(mktemp -d "$TMPROOT/symsize-XXXXXX")"
trap 'rm -rf "$W"' EXIT INT TERM

# ── fixtures: real readelf output, sliced ──────────────────────────────────────────────────────
cat > "$W/sections.txt" <<'SEC'
There are 27 section headers, starting at offset 0x252628:

Section Headers:
  [Nr] Name              Type            Addr     Off    Size   ES Flg Lk Inf Al
  [ 0]                   NULL            00000000 000000 000000 00      0   0  0
  [ 1] .trap             PROGBITS        40380000 001000 000644 00  AX  0   0 256
  [ 4] .rwdata_dummy     NOBITS          3fc80000 00c000 00a0e8 00   A  0   0  4
  [ 5] .data             PROGBITS        3fc8a0e8 00c0e8 003af0 00 WAM  0   0  8
  [ 6] .data.wifi        PROGBITS        3fc8dbd8 00fbd8 0001f0 00  WA  0   0  4
  [ 7] .bss              NOBITS          3fc8ddc8 00fdc8 02b578 00  WA  0   0  8
  [11] .rodata           PROGBITS        3c000120 010120 010f9c 00 AMSR  0   0  8
  [16] .rtc_fast.data    PROGBITS        50000000 0fefaa 000000 00   A  0   0  1
  [19] .stack            NOBITS          3fcb9340 0ff340 0150c0 00  WA  0   0  4
SEC

# Written by a helper so the churn-immunity arm can re-emit the SAME symbols under a DIFFERENT
# set of crate-metadata hashes. If normalisation works, both spellings yield an identical baseline.
emit_symbols(){ # <clock-disambiguator> <phy-disambiguator> > file
  local C="$1" P="$2"
  cat <<SYM

Symbol table '.symtab' contains 51478 entries:
   Num:    Value  Size Type    Bind   Vis      Ndx Name
     0: 00000000     0 NOTYPE  LOCAL  DEFAULT  UND
   700: 3fc9818c 98304 OBJECT  LOCAL  DEFAULT    7 _RNvNvNt${C}_5clock3net9init_heap4HEAP
   261: 3fcb0190 19600 OBJECT  LOCAL  DEFAULT    7 _RNvNvNt${C}_5clock6___main14___embassy_main4POOL
  1011: 3fc8edec 14784 OBJECT  LOCAL  DEFAULT    7 _RNvNt${C}_5clock8ota_mesh14OTA_WINDOW_BUF
  1418: 3fc927ac 14784 OBJECT  LOCAL  DEFAULT    7 _RNvNtNt${C}_5clock3net4mode13GW_OTA_WINDOW
  1019: 3fc8ddec  4096 OBJECT  LOCAL  DEFAULT    7 _RNvNt${C}_5clock3ota12OTA_READBACK
  1795: 3fc8b208  4096 OBJECT  LOCAL  DEFAULT    5 _RNvNt${C}_5clock3ota9OTA_STAGE
 49742: 3fcb79c0  3880 OBJECT  GLOBAL DEFAULT    7 g_cnxMgr
  5519: 3fcb4f30  2008 OBJECT  LOCAL  DEFAULT    7 _RNv${P}_7esp_phy9PHY_STATE
  1315: 3fc8c208  1408 OBJECT  LOCAL  DEFAULT    5 _RNvNtNt${C}_5clock3net4wifi16NTP_SOCK_STORAGE
 49643: 3fcb731c  1308 OBJECT  GLOBAL DEFAULT    7 s_wifi_nvs
 50493: 3fc8d428   960 OBJECT  GLOBAL DEFAULT    5 TxRxCxt
 50010: 3fc8d010   848 OBJECT  GLOBAL DEFAULT    5 phy_param
 50804: 3fcb8f18   816 OBJECT  GLOBAL DEFAULT    7 gWpaSm
  8768: 3fcb68e8   516 OBJECT  LOCAL  DEFAULT    7 _RNvNtNt${C}_5clock3net4wifi9MQTT_JSON
  9001: 3fc8dc00   400 OBJECT  LOCAL  DEFAULT    6 _RNvNtNt${C}_5clock3net4wifi11WIFI_DATSEG
  8762: 3fcb5d3e   424 OBJECT  LOCAL  DEFAULT    7 _RNvNtNt${C}_5clock3net4cast10SCREEN_B64
   181: 3fcb5790  1878 OBJECT  LOCAL  DEFAULT    7 .L_MergedGlobals.1366
   264: 3fc8c7a8  1208 OBJECT  LOCAL  DEFAULT    5 .L_MergedGlobals.1365
  5174: 3fc8a514   416 OBJECT  LOCAL  DEFAULT    5 .Lswitch.table._RNvXsx_NtCs4XY58JHhTKR_7esp_hal4gpio11InputSignal3fmt
  4409: 3c00a1e0  2560 OBJECT  LOCAL  DEFAULT   11 _RNvNtCs2GHBDM79E01_15ed25519_compact12edwards2551912BASEPOINT_PC
  1697: 3c00766c  1560 OBJECT  LOCAL  DEFAULT   11 .Lanon.fc87f6fca5ad39fda60ed8460297fac2.1275
 12345: 3fc8a000    64 OBJECT  LOCAL  DEFAULT    7 _RNvNtNt${C}_5clock3net4wifi9TINY_THING
 12346: 42020020  4096 FUNC    LOCAL  DEFAULT   14 _RNvNt${C}_5clock3net4wifi7service
SYM
}
emit_symbols Cs9FWucCYWq3y CsjD18A8u66C5 > "$W/symbols.txt"

run(){ python3 "$CHECK" --tier t --baseline-dir "$W" --sections "$W/sections.txt" --symbols "$W/symbols.txt" "$@" 2>&1; }

echo "== 1. bless, then round-trip =="
out="$(run --bless)"; rc=$?
eq "bless exits 0" "0" "$rc"
case "$out" in *"16 symbols"*) ok "bless found the 16 tracked statics" ;; *) no "wrong count: $out" ;; esac
case "$out" in *"skipped 3 .L"*) ok "bless reports the 3 skipped .L* symbols" ;; *) no "no skip report: $out" ;; esac
out="$(run)"; rc=$?
eq "unchanged input exits 0" "0" "$rc"
case "$out" in *"matches"*) ok "round-trip says matches" ;; *) no "no match line: $out" ;; esac

B="$W/symbol-sizes.t.txt"

echo "== 2. scoping and filtering — what must NOT be tracked =="
# THE KEEPER ARM. A 2560 B .rodata static is the single largest OBJECT in the fixture. If the
# section filter is dropped or widened, it lands in the baseline and .rodata growth starts failing
# a gate that exists to watch WRITABLE statics (.data/.bss are what come out of the .stack region;
# .rodata is in flash and costs nothing there). "The filter fires" is satisfied by a filter that
# tracks everything — this is what distinguishes the two.
grep -q 'BASEPOINT_PC' "$B" && no "a 2560 B .rodata static was tracked — section scope is not doing anything" \
                            || ok "large .rodata static NOT tracked (section scope is load-bearing)"
grep -q '\.L_MergedGlobals' "$B" && no ".L_MergedGlobals tracked (renumbers every build)" \
                                 || ok ".L_MergedGlobals skipped"
grep -q '\.Lswitch\.table' "$B" && no ".Lswitch.table tracked (the pattern #390 named)" \
                                || ok ".Lswitch.table skipped"
grep -q '\.Lanon' "$B" && no ".Lanon tracked (content-addressed, moves with its constant)" \
                       || ok ".Lanon skipped"
grep -q 'TINY_THING' "$B" && no "a 64 B static was tracked (threshold ignored)" \
                          || ok "below-threshold static excluded"
grep -q 'wifi7service' "$B" && no "a FUNC symbol was tracked (type filter ignored)" \
                            || ok "FUNC symbol excluded (only OBJECT)"
grep -q 'WIFI_DATSEG' "$B" && ok ".data.wifi IS tracked (prefix match, not exact)" \
                           || no ".data.wifi missed — prefix matching is broken"

echo "== 3. normalisation: immune to crate-hash churn =="
grep -q 'Cs\*_' "$B" && ok "baseline stores the placeholder" || no "no placeholder in baseline"
grep -q 'Cs9FWucCYWq3y' "$B" && no "the volatile crate hash is IN the baseline — it will churn" \
                             || ok "volatile crate hash absent from the baseline"
# The design constraint from the issue, tested directly: the SAME statics rebuilt under DIFFERENT
# crate-metadata hashes must compare EQUAL. Without this the baseline is red on every rebuild and
# gets deleted within a week.
cp "$B" "$W/first.txt"
emit_symbols CsZZZZZZZZZZZZ CsQQQQQQQQQQQ > "$W/symbols.txt"
out="$(run)"; rc=$?
eq "different crate hashes still compare EQUAL" "0" "$rc"
run --bless >/dev/null 2>&1
if diff -q "$W/first.txt" "$B" >/dev/null; then ok "baseline is byte-identical under hash churn"; else
  no "baseline differs under hash churn: $(diff "$W/first.txt" "$B" | head -3)"; fi

echo "== 4. drift is detected, with the right symbol and the right delta =="
emit_symbols Cs9FWucCYWq3y CsjD18A8u66C5 > "$W/symbols.txt"
run --bless >/dev/null 2>&1
sed -i 's/^  1315: 3fc8c208  1408 /  1315: 3fc8c208  1472 /' "$W/symbols.txt"   # the real #390 shape
out="$(run)"; rc=$?
eq "a resized static exits 1" "1" "$rc"
case "$out" in *"net +64 B"*) ok "reports the net delta (+64 B)" ;; *) no "no net delta: $out" ;; esac
case "$out" in *"GREW"*"1408"*"1472"*) ok "names the GREW transition 1408 -> 1472" ;; *) no "no transition: $out" ;; esac
case "$out" in *NTP_SOCK_STORAGE*) ok "names the symbol that changed" ;; *) no "does not name the symbol" ;; esac
# and the direction must be distinguishable, or "something changed" is all a reviewer ever learns
sed -i 's/^  1315: 3fc8c208  1472 /  1315: 3fc8c208  1344 /' "$W/symbols.txt"
out="$(run)"; rc=$?
case "$out" in *"SHRANK"*) ok "a shrink is reported as SHRANK, not GREW" ;; *) no "shrink misreported: $out" ;; esac
eq "a shrink also exits 1" "1" "$rc"

echo "== 5. NEW and GONE =="
emit_symbols Cs9FWucCYWq3y CsjD18A8u66C5 > "$W/symbols.txt"
run --bless >/dev/null 2>&1
printf '  99999: 3fcb0000  8192 OBJECT  LOCAL  DEFAULT    7 _RNvNt5clock4newly8ARRIVED\n' >> "$W/symbols.txt"
out="$(run)"; rc=$?
eq "a new large static exits 1" "1" "$rc"
case "$out" in *"NEW"*ARRIVED*) ok "reports NEW with the symbol" ;; *) no "no NEW: $out" ;; esac
emit_symbols Cs9FWucCYWq3y CsjD18A8u66C5 > "$W/symbols.txt"
grep -v 'g_cnxMgr' "$W/symbols.txt" > "$W/s2" && mv "$W/s2" "$W/symbols.txt"
out="$(run)"; rc=$?
eq "a removed static exits 1" "1" "$rc"
case "$out" in *"GONE"*g_cnxMgr*) ok "reports GONE with the symbol" ;; *) no "no GONE: $out" ;; esac

echo "== 6. vacuous-pass guards: cannot-check is 2, never 0 =="
emit_symbols Cs9FWucCYWq3y CsjD18A8u66C5 > "$W/symbols.txt"
out="$(python3 "$CHECK" --tier nope --baseline-dir "$W" --sections "$W/sections.txt" --symbols "$W/symbols.txt" 2>&1)"; rc=$?
eq "missing baseline exits 2" "2" "$rc"
case "$out" in *"not a pass"*) ok "missing baseline says so plainly" ;; *) no "weak message: $out" ;; esac
out="$(run --threshold 999999)"; rc=$?
eq "a filter matching nothing exits 2 (not 0)" "2" "$rc"
case "$out" in *"floor is"*) ok "the floor guard explains itself" ;; *) no "no floor message: $out" ;; esac
echo "garbage" > "$W/badsec.txt"
out="$(python3 "$CHECK" --tier t --baseline-dir "$W" --sections "$W/badsec.txt" --symbols "$W/symbols.txt" 2>&1)"; rc=$?
eq "unparseable sections exits 2" "2" "$rc"
out="$(python3 "$CHECK" --tier t --baseline-dir "$W" --sections "$W/sections.txt" --symbols "$W/badsec.txt" 2>&1)"; rc=$?
eq "unparseable symbols exits 2" "2" "$rc"
out="$(READELF=/nonexistent-readelf python3 "$CHECK" --tier t --baseline-dir "$W" --elf "$W/sections.txt" 2>&1)"; rc=$?
eq "missing readelf exits 2" "2" "$rc"
out="$(python3 "$CHECK" --tier t --baseline-dir "$W" --elf "$W/no-such.elf" 2>&1)"; rc=$?
eq "missing ELF exits 2" "2" "$rc"
# --bless must refuse a vacuous set too, or the guard is bypassable by the one command that
# rewrites the thing being guarded.
out="$(run --bless --threshold 999999)"; rc=$?
eq "--bless also refuses a vacuous set" "2" "$rc"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
