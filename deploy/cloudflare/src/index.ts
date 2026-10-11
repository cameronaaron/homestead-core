// A thin front door: every request goes to the single core container, which
// does its own authentication. Secrets live in Worker secrets
// (`wrangler secret put NAME`) and are handed to the container as its
// environment, never baked into the image.
import { Container, getContainer } from "@cloudflare/containers";
import { EMPTY, type ErrorLog, exposition, record } from "./errors.ts";

/** Fingerprint of the environment a container is started with. */
async function fingerprint(vars: Record<string, string>): Promise<string> {
  const text = JSON.stringify(Object.keys(vars).sort().map((k) => [k, vars[k]]));
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

interface Env {
  CORE: DurableObjectNamespace<Core>;
  DATABASE_URL: string;
  ADMIN_TOKEN?: string;
  DISCORD_WEBHOOK_URL?: string;
  DISCORD_BOT_TOKEN?: string;
  DISCORD_CLIENT_SECRET?: string;
  DISCORD_GUILD_ID?: string;
  LINEAR_API_KEY?: string;
  BUFFER_API_KEY?: string;
  DOCUMENSO_API_TOKEN?: string;
  DOCUMENSO_WEBHOOK_SECRET?: string;
  ZEFFY_WEBHOOK_SECRET?: string;
  RESEND_API_KEY?: string;
  ATTACHMENTS_ACCESS_KEY_ID?: string;
  ATTACHMENTS_SECRET_ACCESS_KEY?: string;
}

/** Secrets the core reads; each enabled integration in homestead.toml
 * refuses to start without its own, so a missing one fails loudly. */
const SECRETS = [
  "DATABASE_URL",
  "ADMIN_TOKEN",
  "DISCORD_WEBHOOK_URL",
  "DISCORD_BOT_TOKEN",
  "DISCORD_CLIENT_SECRET",
  "DISCORD_GUILD_ID",
  "LINEAR_API_KEY",
  "BUFFER_API_KEY",
  "DOCUMENSO_API_TOKEN",
  "DOCUMENSO_WEBHOOK_SECRET",
  "ZEFFY_WEBHOOK_SECRET",
  "RESEND_API_KEY",
  "ATTACHMENTS_ACCESS_KEY_ID",
  "ATTACHMENTS_SECRET_ACCESS_KEY",
] as const;

export class Core extends Container<Env> {
  defaultPort = 8080;
  requiredPorts = [8080];
  // An idle core stops and costs nothing. Anything it was doing (outbox
  // deliveries after a task or harvest) finishes within seconds, well
  // inside this window; failed deliveries retry on the next wake.
  sleepAfter = "10m";
  enableInternet = true; // Postgres, Discord, Linear, Buffer, Documenso

  constructor(ctx: DurableObjectState<{}>, env: Env) {
    super(ctx, env);
    const vars: Record<string, string> = {};
    for (const name of SECRETS) {
      const value = env[name];
      if (value) vars[name] = value;
    }
    this.envVars = vars;
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

  // One call from the Worker per request: the staleness check runs here,
  // inside the Durable Object, instead of as a separate round trip.
  override async fetch(request: Request): Promise<Response> {
    await this.restartIfStale();
    let response: Response;
    try {
      response = await super.fetch(request);
    } catch (error) {
      await this.recordServerError();
      throw error;
    }
    if (response.status >= 500) await this.recordServerError();
    // The core answers /api/v1/metrics only to an authorised caller, so the
    // Worker's own series are appended to that answer and nothing else.
    if (response.ok && new URL(request.url).pathname === "/api/v1/metrics") {
      const log = (await this.ctx.storage.get<ErrorLog>("serverErrors")) ?? EMPTY;
      const headers = new Headers(response.headers);
      headers.delete("content-length");
      return new Response((await response.text()) + exposition(log, Date.now()), {
        status: response.status,
        headers,
      });
    }
    return response;
  }

  /** Durable count of 5xx; see errors.ts for why the core's isn't enough. */
  private async recordServerError() {
    const log = (await this.ctx.storage.get<ErrorLog>("serverErrors")) ?? EMPTY;
    await this.ctx.storage.put("serverErrors", record(log, Date.now()));
  }

  override onError(error: unknown) {
    console.error("core container error", error);
    throw error;
  }
}

const core = (env: Env) => getContainer(env.CORE, "core");

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    return core(env).fetch(request);
  },

  // The daily cron (wrangler.jsonc): wakes the core, whose start-up sends
  // deadline reminders, and checks it's healthy.
  async scheduled(_event: ScheduledController, env: Env): Promise<void> {
    const res = await core(env).fetch(new Request("http://core/healthz"));
    if (!res.ok) throw new Error(`core health check: HTTP ${res.status}`);
  },
} satisfies ExportedHandler<Env>;
