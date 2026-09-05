import type { ReactNode } from 'react';
import { Badge } from '@/components/ui/badge';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Separator } from '@/components/ui/separator';
import { cn } from '@/lib/utils';

interface SlackMessagePreviewProps {
  blocks?: Array<Record<string, unknown>> | null;
  channel?: string;
  error?: string | null;
  eventLabel: string;
  fallbackText?: string;
  isDirty?: boolean;
  isLoading?: boolean;
}

export function SlackMessagePreview({
  blocks,
  channel,
  error,
  eventLabel,
  fallbackText,
  isDirty,
  isLoading,
}: SlackMessagePreviewProps) {
  const hasBlocks = Boolean(blocks?.length);
  const destination = channel?.trim() || '#default-channel';

  return (
    <Card className="overflow-hidden">
      <CardHeader className="border-b bg-muted/20 pb-4">
        <div className="flex items-start justify-between gap-4">
          <div className="space-y-1">
            <CardTitle className="text-base">Message Preview</CardTitle>
            <CardDescription>
              Slack-style rendering for {eventLabel.toLowerCase()} notifications sent to{' '}
              <span className="font-mono text-xs text-foreground">{destination}</span>.
            </CardDescription>
          </div>
          <PreviewStatusBadge isDirty={Boolean(isDirty)} isLoading={Boolean(isLoading)} />
        </div>
      </CardHeader>
      <CardContent className="space-y-4 p-4">
        {error ? (
          <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">
            {error}
          </div>
        ) : null}

        <div className="rounded-2xl border bg-background p-4 shadow-sm">
          <div className="flex gap-3">
            <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-2xl bg-[#4A154B] text-sm font-semibold text-white">
              VT
            </div>
            <div className="min-w-0 flex-1 space-y-3">
              <div>
                <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <span className="text-sm font-semibold text-foreground">QA Platform</span>
                  <span className="text-xs text-muted-foreground">Preview Bot</span>
                </div>
                <div className="mt-1 text-xs text-muted-foreground">{destination}</div>
              </div>

              {hasBlocks ? (
                <div className="space-y-3">
                  {blocks?.map((block, index) => (
                    <SlackBlockRenderer key={`${String(block.type)}-${index}`} block={block} index={index} />
                  ))}
                </div>
              ) : (
                <div className="rounded-xl border border-dashed bg-muted/20 px-4 py-6 text-sm text-muted-foreground">
                  Render a preview to see the full Slack message on this page.
                </div>
              )}
            </div>
          </div>
        </div>

        {fallbackText ? (
          <details className="rounded-lg border bg-muted/10 px-4 py-3">
            <summary className="cursor-pointer text-sm font-medium">Fallback Text</summary>
            <p className="mt-3 whitespace-pre-wrap break-words rounded-md bg-background px-3 py-2 font-mono text-xs text-muted-foreground">
              {fallbackText}
            </p>
          </details>
        ) : null}
      </CardContent>
    </Card>
  );
}

function PreviewStatusBadge({
  isDirty,
  isLoading,
}: {
  isDirty: boolean;
  isLoading: boolean;
}) {
  if (isLoading) {
    return <Badge variant="secondary">Rendering…</Badge>;
  }

  if (isDirty) {
    return <Badge variant="outline">Needs refresh</Badge>;
  }

  return <Badge variant="secondary">Current</Badge>;
}

function SlackBlockRenderer({
  block,
  index,
}: {
  block: Record<string, unknown>;
  index: number;
}) {
  const blockType = typeof block.type === 'string' ? block.type : '';

  if (blockType === 'context') {
    const elements = Array.isArray(block.elements) ? block.elements : [];
    const texts = elements
      .map((element) => {
        if (!element || typeof element !== 'object' || Array.isArray(element)) {
          return null;
        }

        const text = (element as Record<string, unknown>).text;
        return typeof text === 'string' ? text : null;
      })
      .filter((text): text is string => Boolean(text));

    if (!texts.length) {
      return null;
    }

    return (
      <div className="space-y-2">
        {index > 0 ? <Separator /> : null}
        <div className="flex flex-wrap gap-x-3 gap-y-2 text-xs text-muted-foreground">
          {texts.map((text, textIndex) => (
            <span key={`${textIndex}-${text}`} className="break-words">
              <SlackMrkdwn text={text} compact />
            </span>
          ))}
        </div>
      </div>
    );
  }

  const textObject =
    block.text && typeof block.text === 'object' && !Array.isArray(block.text)
      ? (block.text as Record<string, unknown>)
      : null;
  const text = textObject && typeof textObject.text === 'string' ? textObject.text : null;

  if (!text) {
    return null;
  }

  return (
    <div className="space-y-2">
      {index > 0 ? <Separator /> : null}
      <div
        className={cn(
          'space-y-2 text-sm leading-6 text-foreground',
          index === 0 ? 'text-[15px] font-medium' : null,
          index === 1 ? 'text-muted-foreground' : null
        )}
      >
        <SlackMrkdwn text={text} />
      </div>
    </div>
  );
}

function SlackMrkdwn({ compact = false, text }: { compact?: boolean; text: string }) {
  const parts = text.split(/```([\s\S]*?)```/);

  return (
    <>
      {parts.map((part, index) =>
        index % 2 === 1 ? (
          <pre
            key={`code-${index}`}
            className="overflow-x-auto rounded-md bg-muted px-3 py-2 font-mono text-xs text-foreground"
          >
            {part.trim()}
          </pre>
        ) : (
          <SlackParagraphs key={`text-${index}`} compact={compact} text={part} />
        )
      )}
    </>
  );
}

function SlackParagraphs({ compact, text }: { compact: boolean; text: string }) {
  const lines = text.split('\n');

  return (
    <>
      {lines.map((line, index) => {
        const key = `${index}-${line}`;

        if (!line.trim()) {
          return <div key={key} className={compact ? 'h-1' : 'h-2'} />;
        }

        if (line.startsWith('>')) {
          return (
            <blockquote
              key={key}
              className="border-l-2 border-border/80 pl-3 text-muted-foreground"
            >
              {renderInlineMrkdwn(line.replace(/^>\s?/, ''), key)}
            </blockquote>
          );
        }

        return (
          <p key={key} className="whitespace-pre-wrap break-words">
            {renderInlineMrkdwn(line, key)}
          </p>
        );
      })}
    </>
  );
}

function renderInlineMrkdwn(text: string, keyPrefix: string): ReactNode[] {
  const tokenPattern =
    /(`[^`\n]+`)|(\*[^*\n]+\*)|(_[^_\n]+_)|(~[^~\n]+~)|(<https?:\/\/[^>|]+(?:\|[^>]+)?>)/g;
  const nodes: ReactNode[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;

  while ((match = tokenPattern.exec(text)) !== null) {
    if (match.index > lastIndex) {
      nodes.push(text.slice(lastIndex, match.index));
    }

    const token = match[0];
    const tokenKey = `${keyPrefix}-${match.index}`;

    if (token.startsWith('`')) {
      nodes.push(
        <code
          key={tokenKey}
          className="rounded bg-muted px-1.5 py-0.5 font-mono text-[0.85em] text-foreground"
        >
          {token.slice(1, -1)}
        </code>
      );
    } else if (token.startsWith('*')) {
      nodes.push(<strong key={tokenKey}>{token.slice(1, -1)}</strong>);
    } else if (token.startsWith('_')) {
      nodes.push(<em key={tokenKey}>{token.slice(1, -1)}</em>);
    } else if (token.startsWith('~')) {
      nodes.push(<span key={tokenKey} className="line-through">{token.slice(1, -1)}</span>);
    } else if (token.startsWith('<')) {
      const linkMatch = token.match(/^<(https?:\/\/[^>|]+)(?:\|(.+))?>$/);
      if (linkMatch) {
        const href = linkMatch[1];
        const label = linkMatch[2] ?? linkMatch[1];
        nodes.push(
          <a
            key={tokenKey}
            href={href}
            target="_blank"
            rel="noreferrer"
            className="text-primary underline underline-offset-2"
          >
            {label}
          </a>
        );
      } else {
        nodes.push(token);
      }
    }

    lastIndex = tokenPattern.lastIndex;
  }

  if (lastIndex < text.length) {
    nodes.push(text.slice(lastIndex));
  }

  return nodes;
}
