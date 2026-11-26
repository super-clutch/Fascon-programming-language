#!/usr/bin/env python3
"""virtio-net の相手役 / the peer for the virtio-net example

  python3 tests/net_peer.py <fascon> <examples/kernel_net.fsc> [--out-dir DIR]

QEMU の `-netdev socket,udp=...` は、生の Ethernet フレームを UDP のデータグラム 1 個に 1 つずつ入れて流す。このスクリプトは
127.0.0.1 の UDP ソケットで相手役（10.0.2.2）になり、ARP 要求と ICMP エコー要求を送って、応答を 1 バイトずつ確かめる。
root 権限も TAP も要らないので、Windows・Linux・macOS で同じように動く。
QEMU's `-netdev socket,udp=...` carries one raw Ethernet frame per UDP datagram. This script plays the peer (10.0.2.2)
on a UDP socket on 127.0.0.1: it sends an ARP request and ICMP echo requests and checks every byte of the replies.
No root and no TAP device are needed, so it runs the same on Windows, Linux and macOS.
"""
import os
import socket
import struct
import subprocess
import sys
import time

GUEST_MAC = bytes.fromhex("525400123456")
GUEST_IP = bytes([10, 0, 2, 15])
PEER_MAC = bytes.fromhex("52550a000202")
PEER_IP = bytes([10, 0, 2, 2])


def csum(b):
    if len(b) % 2:
        b += b"\0"
    s = sum(struct.unpack("!%dH" % (len(b) // 2), b))
    while s >> 16:
        s = (s & 0xFFFF) + (s >> 16)
    return (~s) & 0xFFFF


def arp_request():
    eth = b"\xff" * 6 + PEER_MAC + b"\x08\x06"
    arp = struct.pack("!HHBBH", 1, 0x0800, 6, 4, 1) + PEER_MAC + PEER_IP + b"\0" * 6 + GUEST_IP
    return eth + arp + b"\0" * 18  # 60 バイトまで詰める / padded to 60 bytes


def echo_request(seq, payload):
    icmp = struct.pack("!BBHHH", 8, 0, 0, 0x4653, seq) + payload
    icmp = icmp[:2] + struct.pack("!H", csum(icmp)) + icmp[4:]
    ip = struct.pack("!BBHHHBBH", 0x45, 0, 20 + len(icmp), seq, 0, 64, 1, 0) + PEER_IP + GUEST_IP
    ip = ip[:10] + struct.pack("!H", csum(ip)) + ip[12:]
    return GUEST_MAC + PEER_MAC + b"\x08\x00" + ip + icmp


def free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    fascon, src = sys.argv[1], sys.argv[2]
    out_dir = sys.argv[sys.argv.index("--out-dir") + 1] if "--out-dir" in sys.argv else None
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    peer_port = sock.getsockname()[1]
    guest_port = free_port()
    cmd = [fascon, "qemu", src, "--net", "udp:%d:%d" % (peer_port, guest_port), "--timeout", "60"]
    if out_dir:
        cmd += ["--out-dir", out_dir]
    proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    guest = ("127.0.0.1", guest_port)
    problems = []

    def exchange(frame, want, tries, gap):
        """frame を送り、want(応答) が真になるフレームを待つ / send frame and wait for a frame accepted by want"""
        for _ in range(tries):
            if proc.poll() is not None:
                return None
            sock.sendto(frame, guest)
            sock.settimeout(gap)
            end = time.time() + gap
            while time.time() < end:
                try:
                    data, _ = sock.recvfrom(4096)
                except socket.timeout:
                    break
                if want(data):
                    return data
        return None

    try:
        # ARP: ゲストが起動してドライバが上がるまで繰り返す / repeated until the guest has booted and its driver is up
        rep = exchange(arp_request(), lambda d: len(d) >= 42 and d[12:14] == b"\x08\x06" and d[20:22] == b"\x00\x02", 150, 0.3)
        if rep is None:
            problems.append("no ARP reply")
        else:
            if rep[0:6] != PEER_MAC or rep[6:12] != GUEST_MAC:
                problems.append("ARP reply: wrong Ethernet addresses")
            if rep[22:28] != GUEST_MAC or rep[28:32] != GUEST_IP or rep[32:38] != PEER_MAC or rep[38:42] != PEER_IP:
                problems.append("ARP reply: wrong sender / target fields")
            print("ARP reply: 10.0.2.15 is at %s" % rep[22:28].hex(":"))
        if not problems:
            for seq, size in ((1, 32), (2, 301), (3, 1400)):
                payload = bytes((seq * 31 + i) % 256 for i in range(size))
                req = echo_request(seq, payload)
                rep = exchange(req, lambda d, s=seq: len(d) >= 42 and d[12:14] == b"\x08\x00" and d[23] == 1 and d[34] == 0 and d[40:42] == struct.pack("!H", s), 10, 1.0)
                if rep is None:
                    problems.append("no echo reply for seq %d" % seq)
                    break
                rep = rep[: 14 + struct.unpack("!H", rep[16:18])[0]]
                ok = rep[0:6] == PEER_MAC and rep[6:12] == GUEST_MAC and rep[26:30] == GUEST_IP and rep[30:34] == PEER_IP
                ok = ok and csum(rep[14:34]) == 0 and csum(rep[34:]) == 0 and rep[42:] == payload and len(rep) == len(req)
                if not ok:
                    problems.append("echo reply for seq %d is wrong" % seq)
                print("echo reply seq=%d: %d bytes of payload returned intact, checksums valid" % (seq, size) if ok else "echo reply seq=%d: BAD" % seq)
        try:
            out, _ = proc.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
            out, _ = proc.communicate()
            problems.append("the guest did not exit")
        guest_lines = [l for l in out.replace("\r", "").splitlines() if not l.startswith("[bsp]") and not l.startswith("[fascon]") and not l.startswith("generated:")]
        for l in guest_lines:
            print("guest: " + l)
        if not any("echo replies=3" in l for l in guest_lines):
            problems.append("the guest did not report 3 echo replies")
    finally:
        if proc.poll() is None:
            proc.kill()
        sock.close()
    for p in problems:
        print("FAIL: " + p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
