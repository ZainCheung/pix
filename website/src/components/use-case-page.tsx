import { Header } from '#/components/header'
import { Footer } from '#/components/footer'

export type UseCaseFaq = {
  question: string
  answer: string
}

export type UseCaseSection = {
  heading: string
  body?: string
  items?: string[]
  steps?: string[]
}

export type UseCase = {
  slug: string
  title: string
  description: string
  eyebrow: string
  h1: string
  intro: string
  sections: UseCaseSection[]
  faq: UseCaseFaq[]
  related: Array<{ href: string; label: string }>
}

export const USE_CASES: Record<string, UseCase> = {
  'pi-from-iphone': {
    slug: 'pi-from-iphone',
    title: 'Use Pi from Your iPhone | Pix',
    description:
      'Use the Pi coding agent from your iPhone while Pi, your code, and your sessions stay on your Mac or Linux machine.',
    eyebrow: '01 / Pi on iPhone',
    h1: 'Use Pi from your iPhone',
    intro:
      'Pix connects your iPhone to the Pi coding agent already running on your Mac or Linux machine. Continue existing sessions, start new ones in authorized workspaces, and control Pi without moving your workspace to the cloud.',
    sections: [
      {
        heading: 'What you can do',
        items: [
          'Continue existing native Pi sessions.',
          'Start a session in a workspace you authorized on the host.',
          'Send prompts and supported image attachments from your phone.',
          'Follow agent progress and review responses while you are away from the desk.',
        ],
      },
      {
        heading: 'How it works',
        steps: [
          'Install Pix on the Mac or Linux machine where Pi runs.',
          'Pair your iPhone with the Pix host.',
          'Choose a workspace, then open an existing Pi session or start a new one.',
        ],
      },
      {
        heading: 'Your code stays on your computer',
        body:
          'Pi reads your repository, runs tools, and owns the native session on your machine. Pix acts as the remote client. Device pairing and workspace authorization gate access before your phone can control a session.',
      },
    ],
    faq: [
      {
        question: 'Does Pix replace Pi?',
        answer:
          'No. Pi remains the coding agent running on your Mac or Linux machine. Pix provides the iPhone interface for those Pi sessions.',
      },
      {
        question: 'Can I continue an existing Pi session?',
        answer:
          'Yes. Pix lists the native Pi sessions already stored on the host so you can resume one from your iPhone.',
      },
      {
        question: 'Does the host computer need to stay on?',
        answer:
          'Yes. Pi and the Pix host run on that computer, so it must be running and reachable while you use Pix.',
      },
    ],
    related: [
      { href: '/docs/installation', label: 'Install Pix' },
      { href: '/docs/remote-access', label: 'Choose a connection path' },
      { href: '/use-cases/continue-pi-sessions', label: 'Continue Pi sessions' },
    ],
  },
  'remote-pi': {
    slug: 'remote-pi',
    title: 'Remote Access for Pi Coding Agent | Pix',
    description:
      'Access a Pi coding session from your phone over a direct LAN connection or an encrypted relay.',
    eyebrow: '02 / Remote access',
    h1: 'Use Pi remotely',
    intro:
      'Pix keeps Pi on your Mac or Linux machine and chooses a direct local connection or an encrypted relay based on where your devices are.',
    sections: [
      {
        heading: 'On the same network',
        body:
          'When your iPhone and computer share a network, Pix discovers the host with Bonjour and connects directly over TCP. The relay is not involved in this path.',
      },
      {
        heading: 'Away from home',
        body:
          'When direct access is unavailable, the host opens an outbound WebSocket connection to the configured relay. The relay authenticates channel roles and forwards opaque encrypted frames. It does not run Pi or store your session content.',
      },
      {
        heading: 'A remote session still runs locally',
        steps: [
          'Leave the Mac or Linux host running with Pi and the Pix service available.',
          'Pair your iPhone and authorize the workspace you want to use.',
          'Open or resume a Pi session from Pix. The session process and files remain on the host.',
        ],
      },
    ],
    faq: [
      {
        question: 'Which connection does Pix use?',
        answer:
          'Pix connects directly over the local network when it can. It uses the configured encrypted relay when the devices are on different networks.',
      },
      {
        question: 'Can the relay read my prompts or code?',
        answer:
          'No. The relay forwards opaque encrypted frames and does not terminate the Pix secure channel.',
      },
      {
        question: 'What happens if the relay goes offline?',
        answer:
          'The phone loses reachability until the connection returns, but Pi and the local session continue running on the host.',
      },
    ],
    related: [
      { href: '/docs/remote-access', label: 'Read remote access documentation' },
      { href: '/docs/installation', label: 'Install the host' },
      { href: '/use-cases/local-first-ai-coding', label: 'See the local-first model' },
    ],
  },
  'continue-pi-sessions': {
    slug: 'continue-pi-sessions',
    title: 'Continue Pi Sessions from Your Phone | Pix',
    description:
      'Resume native Pi coding sessions from your phone without copying your repository or starting a second hosted session.',
    eyebrow: '03 / Native sessions',
    h1: 'Continue Pi sessions from your phone',
    intro:
      'Start Pi at your desk, leave the machine running, and reopen the same native session in Pix when you move. Pix gives you a remote view of Pi instead of creating a separate cloud session.',
    sections: [
      {
        heading: 'The session stays with Pi',
        body:
          'Pi remains the agent and the owner of the native JSONL session on your computer. Pix Host connects the paired client to that runtime, so your existing context stays where Pi created it.',
      },
      {
        heading: 'A simple handoff',
        steps: [
          'Start a Pi session on your Mac or Linux machine.',
          'Run Pix and pair your iPhone with the host.',
          'Open the same workspace and session in Pix when you leave your desk.',
        ],
      },
      {
        heading: 'Keep the local context',
        items: [
          'The repository stays on the host computer.',
          'Pi keeps its native session history and runtime state.',
          'Workspace access remains limited to roots you authorize.',
          'You can release a runtime before another Pi process resumes it.',
        ],
      },
    ],
    faq: [
      {
        question: 'Does Pix create a second session?',
        answer:
          'No. Pix connects to the native Pi sessions on your computer instead of moving them into a hosted session store.',
      },
      {
        question: 'Can I start on my computer and continue on my phone?',
        answer:
          'Yes. Keep the host running, then choose the same workspace and Pi session from Pix.',
      },
      {
        question: 'What if the phone disconnects?',
        answer:
          'The local Pi runtime remains on the host. Pix can reconnect when the host and phone are reachable again.',
      },
    ],
    related: [
      { href: '/docs/cli', label: 'Inspect session commands' },
      { href: '/docs/troubleshooting', label: 'Troubleshoot a connection' },
      { href: '/use-cases/pi-from-iphone', label: 'Use Pi from your iPhone' },
    ],
  },
  'local-first-ai-coding': {
    slug: 'local-first-ai-coding',
    title: 'Local-First Remote AI Coding with Pi | Pix',
    description:
      'Control a local Pi coding agent remotely while code, credentials, tools, and session files stay on your Mac or Linux machine.',
    eyebrow: '04 / Local-first coding',
    h1: 'Remote AI coding without moving your code to the cloud',
    intro:
      'Pix gives your iPhone a remote control for Pi while execution remains on your Mac or Linux machine. Pi reads the workspace and runs tools locally; Pix carries the paired client connection.',
    sections: [
      {
        heading: 'Pi does the work locally',
        items: [
          'Pi reads your repository on the host machine.',
          'Your local credentials and development tools stay beside the code.',
          'Pi processes and native session files remain on the host.',
        ],
      },
      {
        heading: 'Access is explicit',
        body:
          'The host exposes only workspace roots you authorize and accepts only devices you pair. You can revoke a paired client without creating a Pix account or moving your workspace to a hosted service.',
      },
      {
        heading: 'Remote transport stays bounded',
        body:
          'Pix uses a direct local connection when possible. Its relay forwards encrypted frames when you are away. The relay does not run Pi, browse your filesystem, or store application payloads.',
      },
    ],
    faq: [
      {
        question: 'Which coding agent does Pix support?',
        answer: 'Pix connects to Pi. It is a remote client for the Pi coding agent, not a general client for other coding agents.',
      },
      {
        question: 'Where do my credentials stay?',
        answer: 'Credentials remain on the Mac or Linux host where Pi runs. Pix does not copy them to the phone or relay.',
      },
      {
        question: 'Does Pix require a cloud account?',
        answer: 'No. Pix uses explicit device pairing and host workspace authorization instead of a hosted account system.',
      },
    ],
    related: [
      { href: '/docs/architecture', label: 'Read the host architecture' },
      { href: '/docs/remote-access', label: 'Review transport options' },
      { href: '/use-cases/remote-pi', label: 'Use Pi remotely' },
    ],
  },
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
