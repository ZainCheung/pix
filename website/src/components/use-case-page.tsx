import { Footer } from '#/components/footer'
import { Header } from '#/components/header'

export type UseCaseFaq = {
  question: string
  answer: string
}

export type UseCaseLink = {
  href: string
  label: string
}

export type UseCaseContextLink = UseCaseLink & {
  before: string
  after?: string
}

export type UseCaseTable = {
  caption: string
  headers: [string, string]
  rows: Array<[string, string]>
}

export type UseCaseDiagramVariant = 'iphone' | 'network' | 'session' | 'boundary'

export type UseCaseMedia =
  | {
      type: 'image'
      src: string
      alt: string
      caption: string
    }
  | {
      type: 'video'
      src: string
      alt: string
      caption?: string
    }
  | {
      type: 'diagram'
      variant: UseCaseDiagramVariant
      alt: string
      caption: string
    }

export type UseCaseSection = {
  heading: string
  body?: string
  items?: string[]
  steps?: string[]
  table?: UseCaseTable
  contextLinks?: UseCaseContextLink[]
}

export type UseCase = {
  slug: string
  title: string
  description: string
  eyebrow: string
  h1: string
  intro: string
  media?: UseCaseMedia
  sections: UseCaseSection[]
  faq: UseCaseFaq[]
  related: UseCaseLink[]
}

export const USE_CASES: Record<string, UseCase> = {
  'pi-from-iphone': {
    slug: 'pi-from-iphone',
    title: 'Pi Coding Agent on iPhone: Use Pi Remotely | Pix',
    description:
      'Use Pi coding agent on an iPhone with Pix while Pi runs on your Mac or Linux host and native sessions stay on that computer.',
    eyebrow: '01 / Pi on iPhone',
    h1: 'Use Pi Coding Agent from your iPhone',
    intro:
      'Yes—with Pix, Pi continues running on your Mac or Linux machine while your iPhone acts as the remote client. Start or resume a native Pi session from an authorized workspace without moving the Pi runtime to your phone.',
    media: {
      type: 'image',
      src: '/pix-overview.png',
      alt: 'Pix overview showing an iPhone controlling Pi on a Mac or Linux computer through a direct or encrypted connection.',
      caption: 'Pi runs on your computer; Pix moves the control surface to your iPhone.',
    },
    sections: [
      {
        heading: 'Can you run Pi Coding Agent on an iPhone?',
        body:
          'You can use Pi from an iPhone, but Pix does not move or reimplement the Pi runtime on iOS. Pi, its tools, and the session process stay on the host computer; Pix gives you a paired interface to that host.',
      },
      {
        heading: 'What can you do from your iPhone?',
        items: [
          'Resume an existing native Pi session in an authorized workspace.',
          'Start a new Pi session without opening a terminal at your desk.',
          'Send prompts and supported image attachments from your phone.',
          'Follow agent progress, tool activity, and responses while you are away.',
        ],
      },
      {
        heading: 'How to use Pi from iPhone',
        steps: [
          'Install Pi and verify that it runs on your Mac or Linux machine.',
          'Install Pix Host on that same machine and authorize the workspace you want to use.',
          'Pair your iPhone with the host and approve the pairing request.',
          'Open the workspace in Pix, then choose an existing Pi session or start a new one.',
        ],
      },
      {
        heading: 'What stays on your computer?',
        body:
          'The repository, Pi runtime, development tools, credentials, and native session files remain on the host. Pix provides the paired control and transport path; it does not create a cloud copy of your workspace.',
        contextLinks: [
          {
            before: "For the complete data boundary, read Pix's",
            href: '/use-cases/local-first-ai-coding',
            label: 'local-first security explanation',
          },
        ],
      },
      {
        heading: 'Using Pi away from home',
        body:
          'On the same network, Pix can connect directly. On different networks, the host makes an outbound connection to an encrypted relay, so you can reach the same Pi session without exposing an inbound router port.',
        contextLinks: [
          {
            before: 'See how direct LAN and relay transport work in',
            href: '/use-cases/remote-pi',
            label: 'remote access to Pi Coding Agent',
          },
        ],
      },
    ],
    faq: [
      {
        question: 'Can Pi Coding Agent run directly on an iPhone?',
        answer:
          'Pix does not run the Pi runtime on the iPhone. Pi runs on your Mac or Linux host, while Pix is the paired iPhone client for that runtime.',
      },
      {
        question: 'Does Pix require my Mac or Linux computer to stay on?',
        answer:
          'Yes. Pi and Pix Host run on that computer, so it must be running and reachable while you use Pix.',
      },
      {
        question: 'Can I use Pi from my iPhone away from home?',
        answer:
          'Yes. Pix connects directly on a shared network and can use the configured encrypted relay when your iPhone and host are on different networks.',
      },
      {
        question: 'Can I send images to Pi from my phone?',
        answer:
          'Pix supports image attachments on its prompt path. Whether Pi can interpret an image depends on the model provider and vision capability configured for that session.',
      },
      {
        question: 'Can I resume an existing Pi session from my iPhone?',
        answer:
          'Yes. Pix lists native Pi sessions discovered on the host, so you can select an existing session instead of starting over.',
      },
      {
        question: 'Does Pix replace the Pi terminal?',
        answer:
          'No. Pi remains the coding agent and can still be used in its terminal. Pix adds a remote iPhone interface for the same host-side workflows.',
      },
    ],
    related: [
      { href: '/docs/installation', label: 'Install Pix and Pi' },
      { href: '/docs/remote-access', label: 'Pair and choose a connection path' },
      { href: '/use-cases/continue-pi-sessions', label: 'Resume the same Pi session' },
    ],
  },
  'remote-pi': {
    slug: 'remote-pi',
    title: 'Remote Access to Pi Coding Agent from Anywhere | Pix',
    description:
      'Access Pi coding agent from another network with direct LAN or outbound encrypted relay transport—without opening a router port.',
    eyebrow: '02 / Remote access',
    h1: 'Access Pi Coding Agent remotely',
    intro:
      'Pix connects your iPhone to Pi on your Mac or Linux machine using the simplest available path: a direct LAN connection nearby or an outbound encrypted relay when the devices are on different networks.',
    media: {
      type: 'diagram',
      variant: 'network',
      alt: 'Network diagram comparing a direct same-network connection with an encrypted relay connection between an iPhone and a Mac or Linux host.',
      caption: 'Same Wi-Fi uses a direct path; different networks use an outbound encrypted relay.',
    },
    sections: [
      {
        heading: 'How Pix connects to Pi remotely',
        body:
          'Both transport paths carry the same encrypted Pix channel after the connection is established. The host and client authenticate the channel; the relay only helps endpoints find and reach each other when a direct path is unavailable.',
      },
      {
        heading: 'On the same network: direct connection',
        body:
          'When your iPhone and computer share a network, Bonjour discovers the Pix host and the client connects directly over TCP. The relay is not involved in this path, but device pairing and workspace authorization still apply.',
        items: [
          'Keep the iPhone and host on the same network.',
          'Choose the nearby host discovered by Bonjour.',
          'Compare the pairing code and approve the request on the host.',
        ],
      },
      {
        heading: 'Away from home: encrypted relay',
        body:
          'For different networks, Pix Host opens an outbound WebSocket connection to the configured relay. The relay authenticates channel roles and forwards opaque encrypted frames; it cannot run Pi or terminate the secure channel.',
        contextLinks: [
          {
            before: 'Review pairing expiry, revoke controls, and relay configuration in the',
            href: '/docs/remote-access',
            label: 'remote access guide',
          },
        ],
      },
      {
        heading: 'What can the relay see?',
        table: {
          caption: 'The relay routes the channel without becoming a Pi data store.',
          headers: ['Relay can', 'Relay cannot'],
          rows: [
            ['Forward encrypted frames', 'Read prompts or model output'],
            ['Authenticate channel roles', 'Read repository contents or code'],
            ['Enforce connection and size limits', 'Browse the host filesystem'],
            ['Route the client to the host', 'Run Pi or create a session'],
            ['Observe transport metadata needed to route', 'Queue, persist, or replay application payloads'],
          ],
        },
      },
      {
        heading: 'What happens when the connection drops?',
        body:
          'A relay or network failure changes reachability, not the local runtime. Pi and its session continue running on the host; Pix can reconnect when the phone and host can reach each other again.',
        contextLinks: [
          {
            before: 'Learn how to return to the same native session in',
            href: '/use-cases/continue-pi-sessions',
            label: 'the session continuity guide',
          },
        ],
      },
      {
        heading: 'Can I self-host the relay?',
        body:
          'Yes. The public relay is a Cloudflare Worker under the repository’s relay/ directory. Deploy your own Worker, configure its wss:// endpoint in Pix, and keep the same content-blind encrypted-channel contract.',
        contextLinks: [
          {
            before: 'Follow the deployment steps in the',
            href: '/docs/remote-access',
            label: 'self-hosted relay documentation',
          },
        ],
      },
    ],
    faq: [
      {
        question: 'Which connection does Pix use?',
        answer:
          'Pix uses a direct Bonjour-discovered TCP connection when the iPhone and host share a network. It uses the configured encrypted relay when they are on different networks.',
      },
      {
        question: 'Do I need to open a port on my router?',
        answer:
          'No. With relay access, Pix Host makes an outbound WebSocket connection, so you do not need to expose an inbound router port. Direct LAN access stays inside your local network.',
      },
      {
        question: 'Can the relay read my prompts or code?',
        answer:
          'No. The relay forwards opaque encrypted frames and does not terminate the Pix secure channel or store application payloads.',
      },
      {
        question: 'What happens if the relay goes offline?',
        answer:
          'The phone loses reachability until the connection returns, but Pi and the local session continue running on the host.',
      },
      {
        question: 'Can I self-host a Pix relay?',
        answer:
          'Yes. Deploy the public Cloudflare Worker in the relay/ directory and configure its wss:// endpoint with Pix. Self-hosting does not change the hosted relay.',
      },
      {
        question: 'How does remote pairing work?',
        answer:
          'The host creates a single-use QR pairing offer. The offer and short pairing channel expire after two minutes; compare the six-digit code on both devices and approve the request on the host.',
      },
    ],
    related: [
      { href: '/docs/remote-access', label: 'Read the remote access documentation' },
      { href: '/docs/installation', label: 'Install the Pix host' },
      { href: '/use-cases/local-first-ai-coding', label: "Understand Pix's data boundary" },
    ],
  },
  'continue-pi-sessions': {
    slug: 'continue-pi-sessions',
    title: 'Resume the Same Pi Coding Agent Session on Your Phone | Pix',
    description:
      'Resume the same native Pi coding agent session on your phone, then return to your terminal with repository and context intact.',
    eyebrow: '03 / Session continuity',
    h1: 'Resume the same Pi session on your phone',
    intro:
      'Start Pi at your desk, leave the host running, and reopen the same native session in Pix when you move. Same session, same context, and same workspace—without creating a separate cloud conversation.',
    media: {
      type: 'diagram',
      variant: 'session',
      alt: 'Session handoff diagram showing a Pi session moving from a desktop terminal to an iPhone in Pix and back to the terminal.',
      caption: 'Leave the desk without leaving the native Pi session behind.',
    },
    sections: [
      {
        heading: 'What happens to your Pi session when you leave your desk?',
        body:
          'Pi keeps running on the Mac or Linux host while Pix gives you a remote view and control surface. The host remains the place where the workspace, runtime, and session process live.',
      },
      {
        heading: "Pix resumes Pi's native session—it does not create a copy",
        body:
          'Pi’s native JSONL session remains the durable source of truth. Pix Host connects the paired client to that host-side runtime instead of copying messages into a second hosted conversation database.',
      },
      {
        heading: 'Desktop → iPhone → desktop',
        steps: [
          'Start a Pi session in the workspace on your Mac or Linux machine.',
          'Run Pix Host and pair your iPhone with the host.',
          'Open the same workspace and session in Pix when you leave your desk.',
          'Return to the terminal later; the native Pi session and its context are still on the host.',
        ],
      },
      {
        heading: 'What if your phone disconnects?',
        body:
          'A disconnected phone changes the client’s reachability, not Pi’s local process. Once the host and phone are reachable again, Pix can reconnect to the host-side session.',
        contextLinks: [
          {
            before: 'For the network behavior behind reconnects, read',
            href: '/use-cases/remote-pi',
            label: 'how remote Pi access works',
          },
        ],
      },
      {
        heading: 'Can I use the Pi TUI and Pix with the same session?',
        body:
          'Pix coordinates host ownership so two Pi processes do not write the same session at once. The Pi TUI bridge and Pix RPC use the same session lock; release or switch ownership explicitly before another process resumes the session.',
        contextLinks: [
          {
            before: 'See the implementation details in the',
            href: '/docs/pi-tui-bridge',
            label: 'Pi TUI bridge guide',
          },
        ],
      },
      {
        heading: 'Keep the local context',
        items: [
          'The repository remains on the authorized host workspace.',
          'Pi keeps its native session history and runtime state there.',
          'Workspace access remains limited to roots you explicitly authorize.',
          'A session can be released before another Pi process resumes it.',
        ],
        contextLinks: [
          {
            before: 'Learn what Pix keeps on the host in the',
            href: '/use-cases/local-first-ai-coding',
            label: 'local-first security boundary',
          },
        ],
      },
    ],
    faq: [
      {
        question: 'How does Pix resume the same Pi session?',
        answer:
          'Pix discovers native Pi sessions on the host and connects the paired client to the selected host-side runtime. Pi’s native JSONL remains the durable session source of truth.',
      },
      {
        question: 'Does Pix create a second session?',
        answer:
          'No. Pix does not move the conversation into a hosted session store; it provides a remote interface to the native Pi session on your computer.',
      },
      {
        question: 'Can I start on my computer and continue on my phone?',
        answer:
          'Yes. Keep the host running, then choose the same workspace and Pi session from Pix on your iPhone.',
      },
      {
        question: 'What if my phone disconnects while Pi is working?',
        answer:
          'Pi keeps running on the host. Pix can reconnect when the phone and host are reachable again, subject to the session’s current ownership state.',
      },
      {
        question: 'Can I use the Pi TUI and Pix with the same session?',
        answer:
          'Yes, with one live writer at a time. Pix and the Pi TUI bridge coordinate ownership through the host session lock so two processes do not write concurrently.',
      },
      {
        question: 'Does the host computer need to stay on?',
        answer:
          'Yes. The Pi runtime and native session are on the host, so the computer must be running and reachable for a remote handoff.',
      },
    ],
    related: [
      { href: '/docs/cli', label: 'Inspect session commands' },
      { href: '/docs/pi-tui-bridge', label: 'Read the Pi TUI bridge guide' },
      { href: '/use-cases/pi-from-iphone', label: 'Use Pi Coding Agent from iPhone' },
    ],
  },
  'local-first-ai-coding': {
    slug: 'local-first-ai-coding',
    title: 'Local-First Remote Access for Pi Coding Agent | Pix',
    description:
      'Use Pi remotely while your repository, credentials, tools, and native sessions stay on your Mac or Linux host—not in Pix infrastructure.',
    eyebrow: '04 / Local-first boundary',
    h1: 'Use Pi remotely while your workspace stays on your computer',
    intro:
      'Pix moves the control surface, not your development environment. Pi reads your authorized workspace and runs tools on your Mac or Linux host; Pix provides the paired iPhone connection. Model requests still follow the provider you configure in Pi.',
    media: {
      type: 'diagram',
      variant: 'boundary',
      alt: 'Data boundary diagram showing the repository and Pi runtime on the host, opaque encrypted frames through the relay, and model requests handled by the provider configured in Pi.',
      caption: 'Pix infrastructure is distinct from the model provider configured for Pi.',
    },
    sections: [
      {
        heading: 'What does “local-first” mean in Pix?',
        body:
          'Local-first describes Pix’s control boundary: Pi, your repository, tools, credentials, and native session files remain on the host computer. Pix Relay can forward encrypted transport frames, but it is not a hosted Pi runtime or workspace store.',
        table: {
          caption: 'Separate the host, Pix relay, and model-provider responsibilities.',
          headers: ['Boundary', 'Responsibility'],
          rows: [
            ['Your Mac or Linux host', 'Runs Pi, reads the workspace, runs tools, and stores native sessions'],
            ['Pix Relay (optional)', 'Routes authenticated encrypted frames without reading application payloads'],
            ['Model provider you choose', 'Receives the model request that Pi sends under that provider’s policy'],
          ],
        },
      },
      {
        heading: 'What stays on your host?',
        items: [
          'Your repository and authorized workspace roots.',
          'The Pi runtime and local development tools it invokes.',
          'Credentials used by Pi on that machine.',
          'Pi’s native JSONL session files and host-side runtime state.',
        ],
      },
      {
        heading: 'What Pix Relay does not store',
        body:
          'The relay forwards only authenticated encrypted frames. It does not decrypt, parse, queue, persist, or replay prompts, code, model output, or session payloads, and it never runs Pi.',
        table: {
          caption: 'The optional relay is a transport component, not a conversation database.',
          headers: ['Pix Relay handles', 'Pix Relay does not handle'],
          rows: [
            ['Channel-role authentication', 'Prompt or code inspection'],
            ['Opaque encrypted-frame forwarding', 'Filesystem browsing'],
            ['Connection and frame limits', 'Pi process execution'],
            ['Endpoint routing', 'Session-payload storage'],
          ],
        },
        contextLinks: [
          {
            before: 'For direct LAN versus relay behavior, see',
            href: '/use-cases/remote-pi',
            label: 'remote access to Pi Coding Agent',
          },
        ],
      },
      {
        heading: 'Where do model requests go?',
        body:
          'Pi may send relevant prompt context to the model provider you configure, such as a remote or local provider. That provider’s data handling is separate from Pix Relay. Pix does not promise that context sent to a selected model provider stays on your computer; check the provider policy for the session you choose.',
      },
      {
        heading: 'Access is explicit',
        body:
          'The host exposes only workspace roots you authorize and accepts only devices you pair. You can revoke a paired client without creating a Pix account or moving the workspace into a hosted service.',
        items: [
          'Authorize canonical workspace roots on the host.',
          'Compare and approve each device pairing request.',
          'Revoke devices you no longer recognize or use.',
        ],
        contextLinks: [
          {
            before: 'Ready to use it? Start with the',
            href: '/docs/installation',
            label: 'Pix installation guide',
          },
        ],
      },
    ],
    faq: [
      {
        question: 'What does local-first mean for Pix?',
        answer:
          'Pi, your repository, tools, credentials, and native sessions stay on your authorized Mac or Linux host. Pix moves the remote control surface to your iPhone.',
      },
      {
        question: 'Does Pix upload my repository to Pix infrastructure?',
        answer:
          'No. Pix Host keeps the repository on the host, and Pix Relay forwards opaque encrypted frames instead of storing workspace or session payloads.',
      },
      {
        question: 'Can code or prompts leave my computer?',
        answer:
          'Pi may send relevant context to the model provider configured for the session. That model-provider path is separate from Pix Relay, which cannot read or store the encrypted Pix application payload.',
      },
      {
        question: 'Where do my credentials stay?',
        answer:
          'Credentials remain on the Mac or Linux host where Pi runs. Pix does not copy them to the iPhone or relay.',
      },
      {
        question: 'Does Pix require a cloud account?',
        answer:
          'No. Pix uses explicit device pairing and host workspace authorization instead of a hosted account system.',
      },
      {
        question: 'Which coding agent does Pix support?',
        answer:
          'Pix connects to Pi. It is a remote client for the Pi coding agent, not a general client for other coding agents.',
      },
    ],
    related: [
      { href: '/docs/architecture', label: 'Read the host architecture' },
      { href: '/docs/remote-access', label: 'Review transport and relay options' },
      { href: '/docs/installation', label: 'Install Pix' },
      { href: '/use-cases/remote-pi', label: 'Access Pi from another network' },
    ],
  },
}

function UseCaseMediaView({ media }: { media: UseCaseMedia }) {
  if (media.type === 'image') {
    return (
      <figure className="use-case-media use-case-media-image">
        <img src={media.src} alt={media.alt} loading="lazy" decoding="async" />
        <figcaption>{media.caption}</figcaption>
      </figure>
    )
  }

  if (media.type === 'video') {
    return (
      <figure className="use-case-media use-case-media-video">
        <video controls preload="metadata" aria-label={media.alt}>
          <source src={media.src} />
          Your browser does not support embedded video.
        </video>
        {media.caption ? <figcaption>{media.caption}</figcaption> : null}
      </figure>
    )
  }

  return (
    <figure className={`use-case-media use-case-media-diagram use-case-diagram-${media.variant}`}>
      <UseCaseDiagram variant={media.variant} alt={media.alt} />
      <figcaption>{media.caption}</figcaption>
    </figure>
  )
}

function UseCaseDiagram({ variant, alt }: { variant: UseCaseDiagramVariant; alt: string }) {
  if (variant === 'network') {
    return (
      <div className="use-case-diagram-canvas" role="img" aria-label={alt}>
        <div className="use-case-diagram-row">
          <span className="use-case-diagram-label">Same Wi-Fi</span>
          <span className="use-case-diagram-node">iPhone<br /><small>Pix app</small></span>
          <span className="use-case-diagram-connector">direct TCP</span>
          <span className="use-case-diagram-node">Mac / Linux<br /><small>Pix Host + Pi</small></span>
        </div>
        <div className="use-case-diagram-row">
          <span className="use-case-diagram-label">Different networks</span>
          <span className="use-case-diagram-node">iPhone<br /><small>Pix app</small></span>
          <span className="use-case-diagram-connector">encrypted → relay → encrypted</span>
          <span className="use-case-diagram-node">Mac / Linux<br /><small>Pix Host + Pi</small></span>
        </div>
      </div>
    )
  }

  if (variant === 'session') {
    return (
      <div className="use-case-diagram-canvas" role="img" aria-label={alt}>
        <span className="use-case-diagram-session-note">same native Pi session</span>
        <div className="use-case-diagram-session-flow">
          <span className="use-case-diagram-node">Terminal<br /><small>at your desk</small></span>
          <span className="use-case-diagram-connector">leave desk →</span>
          <span className="use-case-diagram-node">iPhone<br /><small>Pix remote client</small></span>
          <span className="use-case-diagram-connector">→ return</span>
          <span className="use-case-diagram-node">Terminal<br /><small>same workspace</small></span>
        </div>
      </div>
    )
  }

  if (variant === 'boundary') {
    return (
      <div className="use-case-diagram-canvas" role="img" aria-label={alt}>
        <div className="use-case-diagram-boundary-grid">
          <div className="use-case-diagram-boundary-card use-case-diagram-boundary-host">
            <strong>Your host</strong>
            <span>repository · credentials</span>
            <span>Pi · tools · sessions</span>
          </div>
          <span className="use-case-diagram-connector">encrypted frames</span>
          <div className="use-case-diagram-boundary-card">
            <strong>Pix Relay</strong>
            <span>route only</span>
            <span>no payload store</span>
          </div>
          <span className="use-case-diagram-connector">Pi sends request</span>
          <div className="use-case-diagram-boundary-card use-case-diagram-boundary-provider">
            <strong>Chosen provider</strong>
            <span>model request</span>
            <span>provider policy</span>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="use-case-diagram-canvas" role="img" aria-label={alt}>
      <div className="use-case-diagram-phone-flow">
        <span className="use-case-diagram-node">iPhone<br /><small>Pix app</small></span>
        <span className="use-case-diagram-connector">paired encrypted channel</span>
        <span className="use-case-diagram-node">Mac / Linux<br /><small>Pi + Pix Host</small></span>
      </div>
    </div>
  )
}

function UseCaseTableView({ table }: { table: UseCaseTable }) {
  return (
    <div className="use-case-table-wrap">
      <table className="use-case-table">
        <caption>{table.caption}</caption>
        <thead>
          <tr>
            <th scope="col">{table.headers[0]}</th>
            <th scope="col">{table.headers[1]}</th>
          </tr>
        </thead>
        <tbody>
          {table.rows.map(([first, second]) => (
            <tr key={`${first}-${second}`}>
              <td>{first}</td>
              <td>{second}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

export function UseCasePage({ page }: { page: UseCase }) {
  return (
    <div className="site-root-v2 use-case-root" id="top">
      <Header />
      <main id="main-content" className="use-case-page">
        <article className="use-case-article">
          <header className="use-case-hero">
            <p className="use-case-eyebrow">{page.eyebrow}</p>
            <h1>{page.h1}</h1>
            <p className="use-case-lede">{page.intro}</p>
          </header>

          {page.media ? <UseCaseMediaView media={page.media} /> : null}

          <div className="use-case-sections">
            {page.sections.map((section) => (
              <section className="use-case-section" key={section.heading}>
                <h2>{section.heading}</h2>
                {section.body ? <p>{section.body}</p> : null}
                {section.items ? (
                  <ul>
                    {section.items.map((item) => <li key={item}>{item}</li>)}
                  </ul>
                ) : null}
                {section.steps ? (
                  <ol>
                    {section.steps.map((step) => <li key={step}>{step}</li>)}
                  </ol>
                ) : null}
                {section.table ? <UseCaseTableView table={section.table} /> : null}
                {section.contextLinks?.map((link) => (
                  <p className="use-case-context-link" key={link.href}>
                    {link.before}{' '}
                    <a href={link.href}>{link.label}</a>{link.after ?? '.'}
                  </p>
                ))}
              </section>
            ))}
          </div>

          <section className="use-case-faq" aria-labelledby="use-case-faq-heading">
            <p className="use-case-eyebrow">Questions</p>
            <h2 id="use-case-faq-heading">Common questions</h2>
            <div className="use-case-faq-list">
              {page.faq.map((item) => (
                <details key={item.question}>
                  <summary>{item.question}</summary>
                  <p>{item.answer}</p>
                </details>
              ))}
            </div>
          </section>

          <nav className="use-case-links" aria-label="Related Pix pages">
            {page.related.map((link) => <a href={link.href} key={link.href}>{link.label}</a>)}
          </nav>
        </article>
      </main>
      <Footer />
    </div>
  )
}
