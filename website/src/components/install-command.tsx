import { Check, Copy, Terminal } from 'lucide-react'
import { useState } from 'react'

import { Button } from '#/components/ui/button'
import { INSTALL_COMMAND } from '#/lib/install'

export function InstallCommand() {
  const [copied, setCopied] = useState(false)

  async function copyInstallCommand() {
    try {
      await navigator.clipboard.writeText(INSTALL_COMMAND)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2_000)
    } catch {
      setCopied(false)
    }
  }

  return (
    <div className="install-command-v2">
      <div className="install-command-toolbar-v2">
        <span className="install-command-language-v2">
          <Terminal size={14} aria-hidden="true" />
          sh
        </span>
      </div>
      <pre tabIndex={0}><code>{INSTALL_COMMAND}</code></pre>
      <Button
        className="copy-button-v2"
        variant="quiet"
        type="button"
        aria-label={copied ? 'Install command copied' : 'Copy install command'}
        title={copied ? 'Install command copied' : 'Copy install command'}
        onClick={copyInstallCommand}
      >
        {copied ? <Check size={15} /> : <Copy size={15} />}
      </Button>
    </div>
  )
}
