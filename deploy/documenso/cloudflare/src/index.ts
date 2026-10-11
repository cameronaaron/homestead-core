// Front door for the single Documenso container. Every string in the
// Worker's environment (the vars in wrangler.jsonc and the secrets) is
// handed to Documenso as its environment.
import { Container, getContainer } from "@cloudflare/containers";

/** Fingerprint of the environment a container is started with. */
async function fingerprint(vars: Record<string, string>): Promise<string> {
  const text = JSON.stringify(Object.keys(vars).sort().map((k) => [k, vars[k]]));
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

interface Env {
  DOCUMENSO: DurableObjectNamespace<Documenso>;
  [name: string]: unknown;
}

export class Documenso extends Container<Env> {
  defaultPort = 3000;
  requiredPorts = [3000];
  // Signers open a link, read, sign: keep it up a while after each request.
  sleepAfter = "15m";
  enableInternet = true; // Supabase, SMTP, and webhooks to the core

  constructor(ctx: DurableObjectState<{}>, env: Env) {
    super(ctx, env);
    this.envVars = Object.fromEntries(
      Object.entries(env).filter((e): e is [string, string] => typeof e[1] === "string"),
    );
  }

  // A running container keeps the environment it started with, and neither
  // `wrangler secret put` nor a rollout replaces it. So the container
  // records what it started with, and the Worker restarts it when the
  // environment has changed since (a new secret takes effect on the next
  // request instead of whenever it next goes idle).
  override async onStart() {
    await this.ctx.storage.put("envFingerprint", await fingerprint(this.envVars ?? {}));
  }

  /** Restarts the container if it's running with an outdated environment. */
  async restartIfStale(): Promise<boolean> {
    const { status } = await this.getState();
    if (status !== "running" && status !== "healthy") return false;
    const started = await this.ctx.storage.get<string>("envFingerprint");
    if (started === (await fingerprint(this.envVars ?? {}))) return false;
    await this.destroy();
    return true;
  }

  // Everything happens here, inside the Durable Object: one call from the
  // Worker per request instead of three. The long start-up wait runs only
  // when the container isn't healthy. `startAndWaitForPorts` on a running
  // container still pings it and calls `onStart` under
  // `blockConcurrencyWhile`, which queued every request (each page asset
  // included) behind the others: 20 parallel requests took ~4.6 s each.
  override async fetch(request: Request): Promise<Response> {
    const restarted = await this.restartIfStale();
    const { status } = await this.getState();
    if (restarted || !this.ctx.container?.running || status !== "healthy") {
      try {
        // A cold start runs Documenso's database migrations before its
        // server listens, which takes far longer than the library's
        // default wait.
        await this.startAndWaitForPorts({
          cancellationOptions: { portReadyTimeoutMS: 120_000, abort: request.signal },
        });
      } catch (error) {
        console.error("documenso container did not start", error);
        return new Response("Signing is starting up. Try again in a minute.", {
          status: 503,
          headers: { "retry-after": "30" },
        });
      }
    }
    return this.containerFetch(request, this.defaultPort);
  }
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    return getContainer(env.DOCUMENSO, "documenso").fetch(request);
  },
} satisfies ExportedHandler<Env>;
