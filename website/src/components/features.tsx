import { Folder, KeyRound, Lock, ShieldCheck, Terminal, Wifi } from 'lucide-react'

const features = [
  {
    icon: <Terminal size={18} />,
    title: 'Pi stays local',
    body: 'Pix launches and supervises the Pi you already use. Your files and native JSONL sessions remain on the host.',
  },
  {
    icon: <Wifi size={18} />,
    title: 'Direct LAN first',
    body: 'Nearby devices use Bonjour discovery and a direct TCP path whenever the network allows it.',
  },
  {
    icon: <Lock size={18} />,
    title: 'Encrypted remotely',
    body: 'The same Pix wire frames travel over the remote path, with the relay forwarding ciphertext only.',
  },
  {
    icon: <Folder size={18} />,
    title: 'Workspaces are explicit',
    body: 'Only canonical workspace roots you authorize are exposed to a paired client.',
  },
  {
    icon: <KeyRound size={18} />,
    title: 'Devices are paired',
    body: 'A client gets access after an explicit approval step on the host, and can be revoked later.',
  },
  {
    icon: <ShieldCheck size={18} />,
    title: 'Open source by default',
    body: 'Host, CLI, protocol fixtures, relay, and the public macOS client are available to inspect.',
  },
]

export function Features() {
  return (
    <section className="features-v2" aria-labelledby="features-heading">
      <div className="features-label-v2" id="features-heading">Why Pix</div>
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
