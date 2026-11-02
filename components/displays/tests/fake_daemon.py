#!/usr/bin/env python3
"""Private stand-in for the way-displays daemon socket (measurement only).
Serves a fixed GET reply with two heads at $XDG_RUNTIME_DIR/way-displays.sock
and logs every request; refuses to run in the live runtime directory."""
import os, socket, sys
runtime = os.environ["XDG_RUNTIME_DIR"]
if runtime == "/run/user/1000":
    sys.exit("refusing to serve in the live session")
REPLY = """CFG:
  ARRANGE: ROW
  ALIGN: TOP
  ORDER:
  - Fake Display A
STATE:
  HEADS:
""" + "".join(f"""  - NAME: {name}
    DESCRIPTION: Fake Display {letter}
    MAKE: Fake
    MODEL: Display {letter}
    MODES:
    - WIDTH: 1920
      HEIGHT: 1080
      REFRESH_MHZ: 60000
    - WIDTH: 1280
      HEIGHT: 720
      REFRESH_MHZ: 60000
    CURRENT:
      ENABLED: true
      SCALE: 1.0
      VRR: false
      MODE:
        WIDTH: 1920
        HEIGHT: 1080
        REFRESH_MHZ: 60000
""" for name, letter in (("HEADLESS-1", "A"), ("HEADLESS-2", "B")))
path = os.path.join(runtime, "way-displays.sock")
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(path)
server.listen(8)
open(sys.argv[1], "w").close()
with open(sys.argv[2], "a") as log:
    while True:
        client, _ = server.accept()
        request = client.recv(1 << 20).decode()
        log.write(request.splitlines()[0] + "\n"); log.flush()
        client.sendall(REPLY.encode())
        client.close()
