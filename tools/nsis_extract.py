"""Minimal NSIS (non-solid) installer extractor using only the stdlib.

Usage: python3 -I tools/nsis_extract.py <installer.exe> <outdir>
Handles deflate and LZMA blocks; NSIS-bzip2 is not implemented.
"""
import sys, struct, zlib, lzma, os, re

src, outdir = sys.argv[1], sys.argv[2]
d = open(src, 'rb').read()
sig = b'\xef\xbe\xad\xdeNullsoftInst'
fh = d.find(sig) - 4
flags, _, _, _, _, hdr_len, total = struct.unpack_from('<7I', d, fh)
data0 = fh + 28

def decompress(blob, expect=None):
    errs = []
    # raw deflate
    try:
        o = zlib.decompressobj(-15).decompress(blob)
        if expect is None or len(o) == expect:
            return o, 'deflate'
    except Exception as e:
        errs.append(('deflate', e))
    # lzma: 5-byte props
    try:
        pb = blob[0]
        lc = pb % 9; pb //= 9; lp = pb % 5; pb //= 5
        dict_size = struct.unpack_from('<I', blob, 1)[0]
        dec = lzma.LZMADecompressor(format=lzma.FORMAT_RAW, filters=[{'id': lzma.FILTER_LZMA1, 'lc': lc, 'lp': lp, 'pb': pb, 'dict_size': dict_size}])
        o = dec.decompress(blob[5:])
        if expect is None or len(o) == expect:
            return o, 'lzma'
    except Exception as e:
        errs.append(('lzma', e))
    raise RuntimeError(f'cannot decompress: {errs}')

def read_block(pos):
    (n,) = struct.unpack_from('<I', d, pos)
    comp = bool(n & 0x80000000); n &= 0x7fffffff
    blob = d[pos+4:pos+4+n]
    if comp:
        out, how = decompress(blob)
    else:
        out, how = blob, 'stored'
    return out, how, pos + 4 + n

hdr, how, datablock = read_block(data0)
print(f'header: {len(hdr)} bytes via {how} (expected {hdr_len}); datablock @ {datablock}')
ints = struct.unpack_from('<%dI' % (len(hdr)//4), hdr)
blocks = [(ints[1+2*i], ints[2+2*i]) for i in range(8)]
ent_off, ent_num = blocks[2]
str_off = blocks[3][0]
str_end = blocks[4][0]
strblk = hdr[str_off:str_end]
unicode = strblk[1:2] == b'\x00' or strblk[:2] == b'\x00\x00'
print('entries', ent_num, 'strings block', len(strblk), 'unicode', unicode)

VARS = {20:'$CMDLINE',21:'$INSTDIR',22:'$OUTDIR',23:'$EXEDIR',24:'$LANGUAGE',25:'$TEMP',26:'$PLUGINSDIR',27:'$EXEPATH',28:'$EXEFILE',29:'$HWNDPARENT',30:'$_CLICK',31:'$_OUTDIR'}
def varname(i):
    if i < 10: return f'${i}'
    if i < 20: return f'$R{i-10}'
    return VARS.get(i, f'$VAR{i}')

def getstr(idx):
    if unicode:
        p = idx*2; out = []
        while True:
            c = struct.unpack_from('<H', strblk, p)[0]; p += 2
            if c == 0: break
            if c in (0xE000, 0xE001, 0xE002, 0xE003):
                v = struct.unpack_from('<H', strblk, p)[0]; p += 2
                if c == 0xE001:
                    out.append(varname(((v >> 8) & 0x7f) << 7 | (v & 0x7f)))
                elif c == 0xE003: out.append(f'$LANGSTR')
                elif c == 0xE002: out.append(f'$SHELL')
                else: out.append(chr(v))
            else:
                out.append(chr(c))
        return ''.join(out)
    else:
        p = idx; out = []
        while True:
            c = strblk[p]; p += 1
            if c == 0: break
            if c >= 0xFC:
                a, b = strblk[p], strblk[p+1]; p += 2
                v = ((b & 0x7f) << 7) | (a & 0x7f)
                if c == 0xFE: out.append(varname(v))
                elif c == 0xFC: out.append('$LANGSTR')
                elif c == 0xFD: out.append('$SHELL')
                else: out.append(chr(a))
            else:
                out.append(chr(c))
        return ''.join(out)

outpath = '$INSTDIR'
seen = {}
count = 0
for i in range(ent_num):
    e = struct.unpack_from('<7i', hdr, ent_off + i*28)
    which = e[0]
    if which == 11:  # CreateDir / SetOutPath
        if e[2]:
            outpath = getstr(e[1])
    elif which == 20:  # ExtractFile
        name = getstr(e[2]); datapos = e[3]
        full = os.path.join(outpath, name).replace('\\', '/')
        full = re.sub(r'[^\w./$ +-]', '_', full)
        if datapos in seen:
            continue
        seen[datapos] = full
        try:
            blob, how, _ = read_block(datablock + datapos)
        except Exception as ex:
            print('FAIL', full, ex); continue
        dst = os.path.join(outdir, full.lstrip('/'))
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        with open(dst, 'wb') as f: f.write(blob)
        count += 1
print('extracted', count, 'files')
