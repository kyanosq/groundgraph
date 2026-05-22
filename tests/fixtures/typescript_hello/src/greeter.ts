import { banner } from "./utils";

export interface GreeterConfig {
  readonly name: string;
  readonly formal?: boolean;
}

export class Greeter {
  constructor(private readonly config: GreeterConfig) {}

  greet(): string {
    const prefix = banner(this.config.formal ?? false);
    return `${prefix}, ${this.config.name}!`;
  }

  goodbye(): string {
    return `Bye, ${this.config.name}!`;
  }
}

export function makeGreeter(name: string, formal = false): Greeter {
  return new Greeter({ name, formal });
}

export async function makeGreeterAsync(name: string): Promise<Greeter> {
  return makeGreeter(name);
}
