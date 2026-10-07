import os
import select
import sys


def main() -> int:
    pid = int(sys.argv[1])
    timeout_ms = int(sys.argv[2])
    descriptor = os.pidfd_open(pid)
    try:
        events = select.poll()
        events.register(descriptor, select.POLLIN)
        print("ARMED", flush=True)
        if not events.poll(timeout_ms):
            return 1
        print("EXITED", flush=True)
        return 0
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    sys.exit(main())
