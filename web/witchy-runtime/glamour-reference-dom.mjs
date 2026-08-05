// A dependency-free instrumented DOM used by Glamour reference tests and
// benchmarks. It implements only the standard operations glamour-dom.mjs uses.

export function createReferenceDom() {
  const operations = Object.create(null);
  const count = (name) => {
    operations[name] = (operations[name] || 0) + 1;
  };

  class ReferenceNode {
    constructor() {
      this.childNodes = [];
      this.parentNode = null;
    }
    appendChild(child) {
      count(child.parentNode ? "moveChild" : "appendChild");
      if (child.parentNode) child.parentNode.removeChild(child);
      child.parentNode = this;
      this.childNodes.push(child);
      return child;
    }
    removeChild(child) {
      count("removeChild");
      const index = this.childNodes.indexOf(child);
      if (index >= 0) this.childNodes.splice(index, 1);
      child.parentNode = null;
      return child;
    }
    replaceChild(next, previous) {
      count("replaceChild");
      const index = this.childNodes.indexOf(previous);
      if (index < 0) throw new Error("replaceChild: old node not found");
      if (next.parentNode) next.parentNode.removeChild(next);
      this.childNodes[index] = next;
      next.parentNode = this;
      previous.parentNode = null;
      return previous;
    }
  }

  class ReferenceText extends ReferenceNode {
    constructor(value) {
      super();
      this._text = String(value);
    }
    get textContent() {
      return this._text;
    }
    set textContent(value) {
      count("setText");
      this._text = String(value);
      this.childNodes = [];
    }
  }

  class ReferenceElement extends ReferenceNode {
    constructor(tag) {
      super();
      this.tagName = String(tag).toUpperCase();
      this.el = String(tag);
      this.attributes = new Map();
      this.listeners = new Map();
    }
    setAttribute(name, value) {
      count("setAttribute");
      this.attributes.set(name, String(value));
    }
    getAttribute(name) {
      return this.attributes.has(name) ? this.attributes.get(name) : null;
    }
    removeAttribute(name) {
      count("removeAttribute");
      this.attributes.delete(name);
    }
    addEventListener(event, listener) {
      count("addEventListener");
      if (!this.listeners.has(event)) this.listeners.set(event, new Set());
      this.listeners.get(event).add(listener);
    }
    removeEventListener(event, listener) {
      count("removeEventListener");
      const listeners = this.listeners.get(event);
      if (listeners) listeners.delete(listener);
    }
    dispatchEvent(event) {
      const listeners = this.listeners.get(event.type);
      if (listeners) for (const listener of [...listeners]) listener(event);
      return true;
    }
    get textContent() {
      return this.childNodes.map((child) => child.textContent).join("");
    }
  }

  const document = {
    createElement(tag) {
      count("createElement");
      return new ReferenceElement(tag);
    },
    createTextNode(value) {
      count("createTextNode");
      return new ReferenceText(value);
    },
  };

  const findAll = (node, tag, found = []) => {
    if (node instanceof ReferenceElement && node.el === tag) found.push(node);
    for (const child of node.childNodes) findAll(child, tag, found);
    return found;
  };

  return {
    document,
    operations,
    createRoot: () => new ReferenceElement("root"),
    findAll,
    snapshotOperations: () => ({ ...operations }),
  };
}
