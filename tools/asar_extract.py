import sys, struct, json, os
src, out = sys.argv[1], sys.argv[2]
f = open(src, 'rb')
hdr = f.read(16)
_, hdr_size, _, json_len = struct.unpack('<4I', hdr)
js = json.loads(f.read(json_len).decode('utf-8', 'replace'))
base = 8 + hdr_size
n = 0
def walk(node, path):
    global n
    for name, v in node.get('files', {}).items():
        p = os.path.join(path, name)
        if 'files' in v:
            os.makedirs(os.path.join(out, p), exist_ok=True); walk(v, p)
        elif 'offset' in v:
            f.seek(base + int(v['offset'])); data = f.read(v['size'])
            dst = os.path.join(out, p); os.makedirs(os.path.dirname(dst), exist_ok=True)
            open(dst, 'wb').write(data); n += 1
        elif v.get('unpacked'):
            print('unpacked:', p)
walk(js, '')
print('files', n)
