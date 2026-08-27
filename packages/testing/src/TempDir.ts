/**
 * A per-daemon temp directory: created under the OS temp dir with a stated
 * prefix, removed recursively and idempotently on teardown.
 *
 * It was `createTempDataRoot`, and "data root" is the retired vocabulary — the
 * `~/.chiefd` tree every company hung under. A company is a DIRECTORY now, and
 * what a harness needs is a disposable one; this makes exactly that and has no
 * opinion about what a caller puts in it.
 *
 * # Why the removal carries a retry budget
 *
 * A teardown removes this tree while the processes that were writing into it
 * are still dying. `tmux kill-server` returns when the CLIENT exits — the
 * server is still reaping each pane's process group, and every one of those
 * panes is an agent home under `<dir>/.chief/agent/*` that may still write a
 * file.
 * A directory that gains an entry between the walk and its own `rmdir` answers
 * `ENOTEMPTY`, and the suite that just passed every one of its tests reports a
 * failure from its own `afterAll` — which is exactly backwards, because
 * nothing about the product went wrong.
 *
 * `ENOTEMPTY` is one of the errors node's own `fs.rm` retries (`EBUSY`,
 * `EMFILE`, `ENFILE`, `ENOTEMPTY`, `EPERM`); it simply defaults to zero
 * retries. Using that budget is the platform's answer to this exact race, and
 * it costs nothing on a tree nobody is touching — the retries only happen when
 * a removal genuinely collides with a writer.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import type { TempDir } from '@/types/TempDir'

const DEFAULT_PREFIX = 'chief-daemon-test-'

export async function createTempDir(prefix?: string): Promise<TempDir> {
  const path = await mkdtemp(join(tmpdir(), prefix ?? DEFAULT_PREFIX))
  let removed = false
  return {
    path,
    async remove(): Promise<void> {
      if (removed) return
      removed = true
      await rm(path, { recursive: true, force: true, maxRetries: 20, retryDelay: 50 })
    }
  }
}
