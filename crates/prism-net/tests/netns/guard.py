"""Sends crafted frames into `bridge prism` guard ports and reports which come out.

usage: guard.py <recv-iface> <name>=<iface>:<hexframe-with-marker> ...
Prints the names of the frames the receiver saw, one per line.
"""
import select
import socket
import sys
import time

recv = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3))
recv.bind((sys.argv[1], 0))
frames = []
for arg in sys.argv[2:]:
    name, rest = arg.split("=", 1)
    iface, hexframe = rest.split(":", 1)
    frames.append((name, iface, bytes.fromhex(hexframe)))

for name, iface, data in frames:
    s = socket.socket(socket.AF_PACKET, socket.SOCK_RAW)
    s.bind((iface, 0))
    s.send(data + b"@@" + name.encode() + b"@@")
    s.close()
    time.sleep(0.05)

seen = set()
end = time.time() + 1.5
while time.time() < end:
    r, _, _ = select.select([recv], [], [], 0.2)
    if not r:
        continue
    pkt = recv.recv(65535)
    for name, _, _ in frames:
        if pkt.endswith(b"@@" + name.encode() + b"@@"):
            seen.add(name)
for name in sorted(seen):
    print(name)
