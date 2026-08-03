// Deterministic browser-authority simulator for Glamour tests.
//
// Time advances only when the test asks. Tasks at the same deadline run in
// creation order. Timer handles and cancellation are observable without
// exposing a real clock or relying on wall time.

export function createHostSimulator(options = {}) {
  let now = Number(options.startTime) || 0;
  let nextHandle = 1;
  let nextOrder = 1;
  const maxSteps = Number(options.maxSteps) || 10_000;
  const tasks = new Map();
  const clears = { timeout: 0, interval: 0 };

  const schedule = (kind, callback, delay) => {
    const handle = nextHandle++;
    const milliseconds = Math.max(0, Number(delay) || 0);
    tasks.set(handle, {
      handle,
      kind,
      callback,
      delay: milliseconds,
      due: now + milliseconds,
      order: nextOrder++,
    });
    return handle;
  };

  const clear = (kind, handle) => {
    const task = tasks.get(handle);
    if (task && task.kind === kind) {
      tasks.delete(handle);
      clears[kind] += 1;
    }
  };

  const ordered = () =>
    [...tasks.values()].sort((left, right) =>
      left.due - right.due || left.order - right.order
    );

  const advanceTo = (targetTime) => {
    const target = Number(targetTime);
    if (!Number.isFinite(target) || target < now) {
      throw new Error("glamour simulator: time must advance monotonically");
    }
    let steps = 0;
    while (true) {
      const task = ordered().find((candidate) => candidate.due <= target);
      if (!task) break;
      if (++steps > maxSteps) {
        throw new Error(`glamour simulator: exceeded ${maxSteps} scheduled steps`);
      }
      now = task.due;
      if (task.kind === "timeout") {
        tasks.delete(task.handle);
      } else {
        task.due += task.delay;
        task.order = nextOrder++;
      }
      task.callback();
    }
    now = target;
    return steps;
  };

  return {
    timerOptions: {
      setTimeout: (callback, delay) => schedule("timeout", callback, delay),
      clearTimeout: (handle) => clear("timeout", handle),
      setInterval: (callback, delay) => schedule("interval", callback, delay),
      clearInterval: (handle) => clear("interval", handle),
    },
    now: () => now,
    advanceBy: (milliseconds) => advanceTo(now + Number(milliseconds)),
    advanceTo,
    pending: (kind = undefined) =>
      ordered()
        .filter((task) => kind === undefined || task.kind === kind)
        .map(({ callback: _callback, ...task }) => ({ ...task })),
    callbackOf: (handle) => tasks.get(handle)?.callback,
    clearCount: (kind) => clears[kind] || 0,
  };
}
