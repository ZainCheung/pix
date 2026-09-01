import { readFileSync, readdirSync, statSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url))
const websiteDirectory = path.resolve(scriptDirectory, '..')
const repositoryDirectory = path.resolve(websiteDirectory, '..')
const docsDirectory = path.join(repositoryDirectory, 'docs')
const sourcePath = path.join(websiteDirectory, 'src', 'lib', 'source.ts')

const failures = []

function report(message) {
  failures.push(message)
}

function walk(directory, predicate = () => true) {
  const files = []
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.name === '.DS_Store') continue
    const absolute = path.join(directory, entry.name)
    if (entry.isDirectory()) {
      files.push(...walk(absolute, predicate))
    } else if (predicate(absolute)) {
      files.push(absolute)
    }
  }
  return files
}

function relativeRepositoryPath(absolute) {
  return path.relative(repositoryDirectory, absolute).split(path.sep).join('/')
}

function sourceFileList() {
  const source = readFileSync(sourcePath, 'utf8')
  const block = source.match(/files:\s*\[([\s\S]*?)\]/)?.[1]
  if (!block) {
    report(`could not find docs.files in ${relativeRepositoryPath(sourcePath)}`)
    return []
  }
  const files = [...block.matchAll(/['"]([^'"]+\.mdx?)['"]/g)].map((match) => match[1])
  if (files.length === 0) report(`docs.files is empty in ${relativeRepositoryPath(sourcePath)}`)
  return files
}

function publicUrl(file) {
  const withoutExtension = file.replace(/\.mdx?$/i, '')
  const segments = withoutExtension
    .split('/')
    .filter((segment) => !(segment.startsWith('(') && segment.endsWith(')')))
    .map((segment) => segment.toLowerCase().replaceAll('_', '-'))
  return segments.join('/') === 'index' ? '/docs' : `/docs/${segments.join('/')}`
}

function readPageTree() {
  const pages = new Map()
  for (const file of sourceFileList()) {
    const absolute = path.join(docsDirectory, file)
    if (!statExists(absolute) || !statSync(absolute).isFile()) {
      report(`source.ts lists a missing docs file: docs/${file}`)
      continue
    }
    const url = publicUrl(file)
    if (pages.has(url)) report(`duplicate public docs URL ${url}`)
    pages.set(url, absolute)
    if (url.includes('(')) report(`public docs URL contains a route group: ${url}`)
  }
  return pages
}

function statExists(absolute) {
  try {
    statSync(absolute)
    return true
  } catch {
    return false
  }
}

function navigationFiles() {
  const rootMeta = path.join(docsDirectory, 'meta.json')
  const allMeta = new Set(
    walk(docsDirectory, (file) => path.basename(file) === 'meta.json'),
  )
  const visitedMeta = new Set()
  const files = new Set()

  function resolveEntry(directory, entry) {
    const candidates = [
      path.join(directory, entry),
      path.join(directory, `${entry}.md`),
      path.join(directory, `${entry}.mdx`),
    ]
    return candidates.find((candidate) => statExists(candidate))
  }

  function visit(metaPath) {
    if (visitedMeta.has(metaPath)) return
    visitedMeta.add(metaPath)
    let metadata
    try {
      metadata = JSON.parse(readFileSync(metaPath, 'utf8'))
    } catch (error) {
      report(`could not parse ${relativeRepositoryPath(metaPath)}: ${error.message}`)
      return
    }
    if (!Array.isArray(metadata.pages)) {
      report(`${relativeRepositoryPath(metaPath)} does not contain a pages array`)
      return
    }
    for (const entry of metadata.pages) {
      if (typeof entry !== 'string') {
        report(`${relativeRepositoryPath(metaPath)} contains a non-string page entry`)
        continue
      }
      const resolved = resolveEntry(path.dirname(metaPath), entry)
      if (!resolved) {
        report(`${relativeRepositoryPath(metaPath)} references missing page or group ${entry}`)
        continue
      }
      if (statSync(resolved).isDirectory()) {
        const nestedMeta = path.join(resolved, 'meta.json')
        if (!statExists(nestedMeta)) {
          report(`navigation group has no meta.json: ${relativeRepositoryPath(resolved)}`)
        } else {
          visit(nestedMeta)
        }
      } else {
        files.add(relativeRepositoryPath(resolved))
      }
    }
  }

  if (!statExists(rootMeta)) {
    report('docs/meta.json is missing')
  } else {
    visit(rootMeta)
  }
  for (const meta of allMeta) {
    if (!visitedMeta.has(meta)) report(`orphan docs navigation file: ${relativeRepositoryPath(meta)}`)
  }
  return files
}

function headingAnchors(content) {
  const anchors = new Set()
  for (const line of content.split(/\r?\n/)) {
    const heading = line.match(/^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$/)?.[1]
    if (!heading) continue
    const plain = heading
      .replace(/<[^>]*>/g, '')
      .replace(/[`*_~]/g, '')
      .trim()
    const base = plain
      .normalize('NFKD')
      .replace(/[\u0300-\u036f]/g, '')
      .toLowerCase()
      .replace(/&/g, ' and ')
      .replace(/[^\p{Letter}\p{Number}\s-]/gu, '')
      .trim()
      .replace(/\s+/g, '-')
      .replace(/-+/g, '-')
    if (base) {
      anchors.add(base)
      anchors.add(base.replaceAll('-', ''))
    }
  }
  for (const match of content.matchAll(/\bid=["']([^"']+)["']/g)) anchors.add(match[1].toLowerCase())
  return anchors
}

function checkAnchor(target, targetFile, origin) {
  const anchor = decodeURIComponent(target.slice(1)).toLowerCase()
  if (!anchor) return
  const anchors = headingAnchors(readFileSync(targetFile, 'utf8'))
  if (!anchors.has(anchor)) {
    report(`${relativeRepositoryPath(origin)} links to missing anchor #${anchor} in ${relativeRepositoryPath(targetFile)}`)
  }
}

function isExternal(target) {
  return /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(target)
}

function checkPublicTarget(target, origin, pages) {
  if (!target.startsWith('/docs')) return
  const [pathname, fragment = ''] = target.split('#', 2)
  const targetFile = pages.get(pathname)
  if (!targetFile) {
    report(`${relativeRepositoryPath(origin)} links to missing public docs page ${pathname}`)
    return
  }
  if (fragment) checkAnchor(`#${fragment}`, targetFile, origin)
}

function checkRelativeTarget(target, origin, pages) {
  if (!target || target.startsWith('#') || target.startsWith('/')) {
    if (target.startsWith('#')) checkAnchor(target, origin, origin)
    else if (target.startsWith('/docs')) checkPublicTarget(target, origin, pages)
    return
  }
  if (isExternal(target)) return
  const [pathname, fragment = ''] = target.split('#', 2)
  const withoutQuery = pathname.split('?', 1)[0]
  const targetFile = path.resolve(path.dirname(origin), withoutQuery)
  if (!statExists(targetFile) || !statSync(targetFile).isFile()) {
    report(`${relativeRepositoryPath(origin)} links to missing repository file ${target}`)
    return
  }
  if (fragment) checkAnchor(`#${fragment}`, targetFile, origin)
}

function checkMarkdownLinks(file, pages) {
  const content = readFileSync(file, 'utf8')
  const linkPattern = /!?\[[^\]]*\]\(\s*(?:<([^>]+)>|([^\s)]+))/g
  for (const match of content.matchAll(linkPattern)) {
    const target = (match[1] ?? match[2] ?? '').trim()
    if (target.startsWith('/docs')) checkPublicTarget(target, file, pages)
    else checkRelativeTarget(target, file, pages)
  }
}

function checkQuotedPublicLinks(file, pages) {
  const content = readFileSync(file, 'utf8')
  const quotedPath = /["'`](\/docs(?:\/[A-Za-z0-9._~!$&'()*+,;=:@%~-]+)*(?:#[A-Za-z0-9._~!$&'()*+,;=:@%~-]+)?)["'`]/g
  for (const match of content.matchAll(quotedPath)) checkPublicTarget(match[1], file, pages)
}

function checkRouteGroupOutput() {
  const generatedFiles = [
    path.join(websiteDirectory, 'src', 'generated', 'sitemap-lastmod.ts'),
    path.join(websiteDirectory, 'public', 'sitemap.xml'),
    path.join(websiteDirectory, 'public', 'llms.txt'),
  ]
  for (const file of generatedFiles) {
    if (!statExists(file)) continue
    const content = readFileSync(file, 'utf8')
    if (/\/docs\/[^\n"<]*(?:\(start\)|\(use-pix\)|\(understand-pix\)|\(advanced\)|\(reference\)|\(develop-pix\))/.test(content)) {
      report(`generated docs URLs contain a Fumadocs route group: ${relativeRepositoryPath(file)}`)
    }
  }
}

const pages = readPageTree()
const navigation = navigationFiles()
for (const file of sourceFileList()) {
  const relative = `docs/${file}`
  if (!navigation.has(relative)) report(`source.ts page is not represented in navigation: ${relative}`)
}

const markdownFiles = walk(repositoryDirectory, (file) => {
  const extension = path.extname(file).toLowerCase()
  return extension === '.md' || extension === '.mdx'
})
  .filter((file) => !file.includes(`${path.sep}node_modules${path.sep}`))
  .filter((file) => !file.includes(`${path.sep}target${path.sep}`))
  .filter((file) => !file.includes(`${path.sep}website${path.sep}dist${path.sep}`))
  // Local planning notes are ignored by Git and may contain draft URLs.
  .filter((file) => !file.includes(`${path.sep}docs${path.sep}ai-development${path.sep}`))

for (const file of markdownFiles) {
  checkMarkdownLinks(file, pages)
  checkQuotedPublicLinks(file, pages)
}

for (const file of walk(path.join(websiteDirectory, 'src'), (candidate) => /\.(?:md|mdx|ts|tsx)$/.test(candidate))) {
  if (file.endsWith(`${path.sep}routeTree.gen.ts`) || file.endsWith(`${path.sep}routes${path.sep}docs${path.sep}$.tsx`)) continue
  checkQuotedPublicLinks(file, pages)
}

checkRouteGroupOutput()

if (failures.length > 0) {
  console.error(`Documentation link check failed with ${failures.length} problem(s):`)
  for (const failure of failures) console.error(`- ${failure}`)
  process.exitCode = 1
} else {
  console.log(`Documentation link check passed (${pages.size} public pages).`)
}
