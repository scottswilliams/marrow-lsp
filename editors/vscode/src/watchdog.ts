// Race a start operation against a timeout. Resolves with the operation's value if it
// finishes first; rejects if the deadline wins. Kept free of vscode so the timing
// contract that guards activation can be exercised directly. The caller decides what to
// do on rejection (surface an error, tear the client down); this only owns the race.

export async function withStartTimeout<T>(start: Promise<T>, timeoutMs: number): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const watchdog = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`server did not start within ${timeoutMs} ms`)),
      timeoutMs,
    );
  });

  try {
    return await Promise.race([start, watchdog]);
  } finally {
    if (timer !== undefined) {
      clearTimeout(timer);
    }
  }
}
