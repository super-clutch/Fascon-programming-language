#!/usr/bin/env python3
"""fmtcheck.py — fmtfuzz.c の出力を確かめる（往復・最短・近い方・形） / check the output of fmtfuzz.c (round trip,
shortest, nearest, shape). usage: fmtcheck.py out.txt  (exit 1 on any mismatch)"""
import struct, sys, re
def f32_of_bits(b): return struct.unpack('<f', struct.pack('<I', b))[0]
def to_f32(x):
    try:
        return struct.unpack('<f', struct.pack('<f', x))[0]
    except OverflowError:
        return float('inf') if x > 0 else float('-inf')
def dec_to_f32(c):
    """the f32 nearest to the Decimal c (ties to even), without double rounding"""
    from decimal import Decimal
    x = to_f32(float(c))
    if x in (float('inf'), float('-inf')):
        return x
    b = struct.unpack('<I', struct.pack('<f', x))[0]
    cands = []
    for d in (-1, 0, 1):
        bb = (b + d) & 0xFFFFFFFF
        y = struct.unpack('<f', struct.pack('<I', bb))[0]
        if y == y and y not in (float('inf'), float('-inf')):
            cands.append((abs(Decimal(y) - c), bb & 1, y))
    cands.sort()
    return cands[0][2]
def sig_digits(s):
    s = s.lstrip('-')
    if 'e' in s: m = s.split('e')[0]
    else: m = s
    m = m.replace('.', '').lstrip('0').rstrip('0')
    return m
bad = 0; n = 0
for line in open(sys.argv[1]):
    parts = line.split()
    if parts[0] == 'f':
        bits = int(parts[1], 16); out = parts[2]; v = f32_of_bits(bits); is32 = True
    else:
        bits = int(parts[0], 16); out = parts[1]; v = struct.unpack('<d', struct.pack('<Q', bits))[0]; is32 = False
    n += 1
    if v != v:
        ok = out == 'NaN'
    elif v in (float('inf'), float('-inf')):
        ok = out == ('inf' if v > 0 else '-inf')
    else:
        back = float(out)
        if is32:
            from decimal import Decimal
            back = dec_to_f32(Decimal(out))
        ok = back == v and (str(back)[0] == '-') == (out[0] == '-') if v == 0 else back == v
        # shortest: compare digit count with Python (f64: repr; f32: minimal %.{p}e that round-trips)
        if ok and v != 0:
            if not is32:
                ref = sig_digits(repr(v))
            else:
                from decimal import Decimal, ROUND_FLOOR, ROUND_CEILING, ROUND_HALF_EVEN
                ex = Decimal(v)
                found = None
                for p in range(1, 10):
                    q = Decimal(1).scaleb(ex.copy_abs().adjusted() - p + 1)
                    cands = [ex.quantize(q, rounding=ROUND_FLOOR), ex.quantize(q, rounding=ROUND_CEILING)]
                    good = [c for c in cands if dec_to_f32(c) == v]
                    if good:
                        good.sort(key=lambda c: (abs(c - ex), int(c.copy_abs().as_tuple().digits[-1]) % 2))
                        found = good[0]
                        break
                ref = sig_digits('%E' % found) if found is not None else ''
                ref = sig_digits(format(found, 'e'))
            mine = sig_digits(out)
            if len(mine) != len(ref):
                ok = False
            elif mine != ref:
                ok = 'tie'
        # shape: decimal if 1e-4 <= |v| < 1e16 (in the value type), else exponent
        a = abs(v)
        want_dec = (a >= (to_f32(1e-4) if is32 else 1e-4)) and (a < (to_f32(1e16) if is32 else 1e16)) or v == 0
        if ('e' in out) == want_dec: ok = False
        if want_dec and '.' not in out: ok = False
    if ok is not True:
        bad += 1
        if bad < 15: print('BAD', line.strip())
print(n, 'checked', bad, 'bad')
sys.exit(1 if bad else 0)
