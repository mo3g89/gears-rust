export type FqlOperator = '=' | '!=' | '~' | '!~' | '>' | '>=' | '<' | '<=' | 'in' | 'not in' | ':';

type TokenType = 'word' | 'string' | 'op' | 'lparen' | 'rparen' | 'comma';

interface Token {
  type: TokenType;
  value: string;
}

interface FqlClause {
  field: string;
  op: FqlOperator;
  values: string[];
}

type FqlNode =
  | { type: 'clause'; clause: FqlClause }
  | { type: 'and'; left: FqlNode; right: FqlNode }
  | { type: 'or'; left: FqlNode; right: FqlNode };

export type FqlAccessor<T> = (item: T, field: string) => string | number | string[] | number[] | null | undefined;

interface FqlCompileResult<T> {
  matches: (item: T) => boolean;
  error?: string;
  mode: 'fql' | 'text';
}

function tokenize(input: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;

  while (i < input.length) {
    const ch = input[i];

    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }

    if (ch === '(') {
      tokens.push({ type: 'lparen', value: ch });
      i += 1;
      continue;
    }
    if (ch === ')') {
      tokens.push({ type: 'rparen', value: ch });
      i += 1;
      continue;
    }
    if (ch === ',') {
      tokens.push({ type: 'comma', value: ch });
      i += 1;
      continue;
    }

    const two = input.slice(i, i + 2);
    if (['!=', '!~', '>=', '<='].includes(two)) {
      tokens.push({ type: 'op', value: two });
      i += 2;
      continue;
    }
    if (['=', '~', '>', '<', ':'].includes(ch)) {
      tokens.push({ type: 'op', value: ch });
      i += 1;
      continue;
    }

    if (ch === '"' || ch === "'") {
      const quote = ch;
      i += 1;
      let value = '';
      while (i < input.length) {
        const current = input[i];
        if (current === '\\' && i + 1 < input.length) {
          value += input[i + 1];
          i += 2;
          continue;
        }
        if (current === quote) {
          i += 1;
          break;
        }
        value += current;
        i += 1;
      }
      tokens.push({ type: 'string', value });
      continue;
    }

    let value = '';
    while (i < input.length) {
      const current = input[i];
      if (/\s/.test(current) || ['(', ')', ',', '=', '!', '~', '>', '<', ':', '"', "'"].includes(current)) {
        break;
      }
      value += current;
      i += 1;
    }
    if (value) {
      tokens.push({ type: 'word', value });
      continue;
    }

    throw new Error(`Unexpected character '${ch}'`);
  }

  return tokens;
}

class Parser {
  private tokens: Token[];
  private pos = 0;

  constructor(tokens: Token[]) {
    this.tokens = tokens;
  }

  parse(): FqlNode {
    const node = this.parseOr();
    if (this.pos < this.tokens.length) {
      throw new Error(`Unexpected token '${this.tokens[this.pos].value}'`);
    }
    return node;
  }

  private peek(offset = 0): Token | undefined {
    return this.tokens[this.pos + offset];
  }

  private matchWord(word: string): boolean {
    const token = this.peek();
    if (!token || token.type !== 'word') return false;
    return token.value.toLowerCase() === word.toLowerCase();
  }

  private consume(): Token {
    const token = this.tokens[this.pos];
    if (!token) throw new Error('Unexpected end of query');
    this.pos += 1;
    return token;
  }

  private expect(type: TokenType): Token {
    const token = this.consume();
    if (token.type !== type) {
      throw new Error(`Expected ${type}, got '${token.value}'`);
    }
    return token;
  }

  private parseOr(): FqlNode {
    let left = this.parseAnd();
    while (this.matchWord('or')) {
      this.consume();
      const right = this.parseAnd();
      left = { type: 'or', left, right };
    }
    return left;
  }

  private parseAnd(): FqlNode {
    let left = this.parsePrimary();
    while (this.matchWord('and')) {
      this.consume();
      const right = this.parsePrimary();
      left = { type: 'and', left, right };
    }
    return left;
  }

  private parsePrimary(): FqlNode {
    if (this.peek()?.type === 'lparen') {
      this.consume();
      const node = this.parseOr();
      this.expect('rparen');
      return node;
    }
    return { type: 'clause', clause: this.parseClause() };
  }

  private parseClause(): FqlClause {
    const fieldToken = this.consume();
    if (!fieldToken || !['word', 'string'].includes(fieldToken.type)) {
      throw new Error('Expected field name');
    }
    const field = fieldToken.value.toLowerCase();

    let op: FqlOperator;
    const current = this.peek();
    if (!current) {
      throw new Error(`Expected operator after '${field}'`);
    }

    if (current.type === 'op') {
      op = this.consume().value as FqlOperator;
    } else if (this.matchWord('in')) {
      this.consume();
      op = 'in';
    } else if (this.matchWord('not') && this.peek(1)?.type === 'word' && this.peek(1)?.value.toLowerCase() === 'in') {
      this.consume();
      this.consume();
      op = 'not in';
    } else {
      throw new Error(`Expected operator after '${field}'`);
    }

    const values: string[] = [];
    if (op === 'in' || op === 'not in') {
      if (this.peek()?.type === 'lparen') {
        this.consume();
        while (true) {
          const valueToken = this.consume();
          if (!valueToken || !['word', 'string'].includes(valueToken.type)) {
            throw new Error(`Expected value in list for '${field}'`);
          }
          values.push(valueToken.value);

          if (this.peek()?.type === 'comma') {
            this.consume();
            continue;
          }
          break;
        }
        this.expect('rparen');
      } else {
        const valueToken = this.consume();
        if (!valueToken || !['word', 'string'].includes(valueToken.type)) {
          throw new Error(`Expected value after '${field} ${op}'`);
        }
        values.push(valueToken.value);
      }
    } else {
      const valueToken = this.consume();
      if (!valueToken || !['word', 'string'].includes(valueToken.type)) {
        throw new Error(`Expected value after '${field} ${op}'`);
      }
      values.push(valueToken.value);
    }

    return { field, op, values };
  }
}

function toStringList(value: unknown): string[] {
  if (value == null) return [];
  if (Array.isArray(value)) return value.map((v) => String(v));
  return [String(value)];
}

function toNumberList(value: unknown): number[] {
  return toStringList(value)
    .map((v) => Number(v))
    .filter((n) => Number.isFinite(n));
}

function evaluateClause<T>(item: T, clause: FqlClause, accessor: FqlAccessor<T>): boolean {
  const actual = accessor(item, clause.field);
  const actualStrings = toStringList(actual).map((v) => v.toLowerCase());
  const values = clause.values.map((v) => v.toLowerCase());
  const firstValue = values[0] || '';

  switch (clause.op) {
    case '=':
      return actualStrings.some((v) => v === firstValue);
    case '!=':
      return actualStrings.every((v) => v !== firstValue);
    case '~':
    case ':':
      return actualStrings.some((v) => v.includes(firstValue));
    case '!~':
      return actualStrings.every((v) => !v.includes(firstValue));
    case 'in':
      return actualStrings.some((v) => values.includes(v));
    case 'not in':
      return actualStrings.every((v) => !values.includes(v));
    case '>':
    case '>=':
    case '<':
    case '<=': {
      const expected = Number(clause.values[0]);
      if (!Number.isFinite(expected)) return false;
      const actualNumbers = toNumberList(actual);
      if (actualNumbers.length === 0) return false;
      return actualNumbers.some((num) => {
        if (clause.op === '>') return num > expected;
        if (clause.op === '>=') return num >= expected;
        if (clause.op === '<') return num < expected;
        return num <= expected;
      });
    }
    default:
      return false;
  }
}

function evaluateNode<T>(item: T, node: FqlNode, accessor: FqlAccessor<T>): boolean {
  if (node.type === 'clause') {
    return evaluateClause(item, node.clause, accessor);
  }
  if (node.type === 'and') {
    return evaluateNode(item, node.left, accessor) && evaluateNode(item, node.right, accessor);
  }
  return evaluateNode(item, node.left, accessor) || evaluateNode(item, node.right, accessor);
}

export function compileFql<T>(
  query: string,
  accessor: FqlAccessor<T>,
  fallbackText: (item: T) => string
): FqlCompileResult<T> {
  const trimmed = query.trim();
  if (!trimmed) {
    return { matches: () => true, mode: 'text' };
  }

  const looksLikeFql = /[=!~<>:]|\b(?:and|or|in|not)\b/i.test(trimmed);
  if (!looksLikeFql) {
    const needle = trimmed.toLowerCase();
    return {
      matches: (item) => fallbackText(item).toLowerCase().includes(needle),
      mode: 'text',
    };
  }

  try {
    const tokens = tokenize(trimmed);
    const ast = new Parser(tokens).parse();
    return {
      matches: (item) => evaluateNode(item, ast, accessor),
      mode: 'fql',
    };
  } catch (error) {
    const needle = trimmed.toLowerCase();
    return {
      matches: (item) => fallbackText(item).toLowerCase().includes(needle),
      error: error instanceof Error ? error.message : 'Invalid FQL',
      mode: 'text',
    };
  }
}
