import { Folder, History, KeyRound, Laptop, Terminal, Wifi } from 'lucide-react'

const features = [
  {
    icon: <Terminal size={18} />,
    title: 'Pi stays Pi',
    body: 'Pix does not replace your coding agent. It connects your phone to the Pi already running on your computer.',
  },
  {
    icon: <History size={18} />,
    title: 'Continue your sessions',
    body: 'Open the Pi sessions already on your computer, resume where you left off, or start a new one from your phone.',
  },
  {
    icon: <Laptop size={18} />,
    title: 'Your machine stays in control',
    body: 'Your workspace, credentials, tools, and Pi processes stay on your Mac or Linux machine. Model requests follow the provider you choose and its data policy.',
  },
  {
    icon: <Wifi size={18} />,
    title: 'Nearby or away',
    body: 'Connect directly on your local network, or use an encrypted relay when you are away from your computer.',
  },
  {
    icon: <Folder size={18} />,
    title: 'Share only what you choose',
    body: 'You decide which workspaces Pix can access. The rest of your filesystem stays out of reach.',
  },
  {
    icon: <KeyRound size={18} />,
    title: 'Pair devices, not accounts',
    body: 'There is no Pix account to create. Pair trusted devices with your host and revoke them whenever you want.',
  },
]

export function Features() {
  return (
    <section className="features-v2" aria-labelledby="features-heading">
      <div className="features-intro-v2">
        <div className="features-label-v2">Why Pix</div>
        <div className="features-heading-row-v2">
          <h2 id="features-heading">Your Pi, wherever you are.</h2>
          <a className="features-link-v2" href="/use-cases">
            Explore use cases <span aria-hidden="true">→</span>
          </a>
        </div>
      </div>
      <div className="features-grid-v2">
        {features.map((feature) => (
          <article className="feature-item-v2" key={feature.title}>
            <div className="feature-title-v2"><span>{feature.icon}</span><h3>{feature.title}</h3></div>
            <p>{feature.body}</p>
          </article>
        ))}
      </div>
    </section>
  )
}
