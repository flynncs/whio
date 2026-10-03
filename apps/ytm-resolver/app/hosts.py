import threading
import time
from collections.abc import Iterator
from contextlib import contextmanager


class HostLimiter:
    def __init__(self) -> None:
        self._pace = threading.Lock()
        self._host_locks: dict[str, threading.Lock] = {}
        self._host_started_at: dict[str, float] = {}

    @contextmanager
    def serialize(self, host: str, minimum_interval: float) -> Iterator[None]:
        with self._pace:
            lock = self._host_locks.setdefault(host, threading.Lock())

        with lock:
            now = time.monotonic()
            if minimum_interval > 0:
                elapsed = now - self._host_started_at.get(host, 0.0)
                if elapsed < minimum_interval:
                    time.sleep(minimum_interval - elapsed)
            self._host_started_at[host] = now
            yield
