import { Folder, History, Image, KeyRound, Laptop, Terminal, Wifi, Wrench } from 'lucide-react'
import type { ReactNode } from 'react'

type Feature = {
  icon: ReactNode
  title: string
  body: string
}

const capabilities: Feature[] = [
  {
    icon: <History size={18} />,
    title: 'Continue sessions',
    body: 'Open the Pi sessions already on your computer, resume where you left off, or start a new one from your phone.',
  },
  {
    icon: <Image size={18} />,
    title: 'Send prompts and images',
    body: 'Talk to Pi from your iPhone. Attach images when the task needs them.',
  },
  {
    icon: <Wrench size={18} />,
    title: 'See tool calls',
    body: 'Watch Pi work: commands, files, and results stay visible while the session runs on your computer.',
  },
  {
    icon: <Wifi size={18} />,
    title: 'Work nearby or away',
    body: 'Connect on your local network, or use an encrypted relay when you are not at home.',
  },
]

const reasons: Feature[] = [
  {
    icon: <Terminal size={18} />,
    title: 'Pi stays Pi',
    body: 'Pix does not replace your coding agent. It connects your phone to the Pi already running on your computer.',
  },
  {
    icon: <Laptop size={18} />,
    title: 'Your machine stays in control',
    body: 'Your workspace, credentials, tools, and Pi processes stay on your Mac or Linux machine. Model requests follow the provider you choose.',
  },
  {
    icon: <KeyRound size={18} />,
    title: 'No Pix account',
    body: 'Pair your iPhone with your computer. You authorize the device, and you can revoke it later.',
  },
  {
    icon: <Folder size={18} />,
    title: 'Local-first',
    body: 'You decide which workspaces Pix can access. The rest of your filesystem stays out of reach.',
  },
]

function FeatureGrid({
  id,
  label,
  heading,
  items,
  link,
}: {
  id: string
  label: string
  heading: string
  items: Feature[]
  link?: { href: string; text: string }
}) {
  return (
    <section className="features-v2" aria-labelledby={id}>
      <div className="features-intro-v2">
        <div className="features-label-v2">{label}</div>
        <div className="features-heading-row-v2">
          <h2 id={id}>{heading}</h2>
          {link ? (
            <a className="features-link-v2" href={link.href}>
              {link.text} <span aria-hidden="true">→</span>
            </a>
          ) : null}
        </div>
      </div>
      <div className="features-grid-v2" data-cols="2">
        {items.map((feature) => (
          <article className="feature-item-v2" key={feature.title}>
            <div className="feature-title-v2"><span>{feature.icon}</span><h3>{feature.title}</h3></div>
            <p>{feature.body}</p>
          </article>
        ))}
      </div>
    </section>
  )
}

export function Capabilities() {
  return (
    <FeatureGrid
      id="capabilities-heading"
      label="What you can do"
      heading="Your Pi, in your pocket."
      items={capabilities}
      link={{ href: '/use-cases', text: 'Explore use cases' }}
    />
  )
}

export function WhyPix() {
  return (
    <FeatureGrid
      id="features-heading"
      label="Why Pix"
      heading="Keep Pi where it belongs."
      items={reasons}
    />
  )
}
