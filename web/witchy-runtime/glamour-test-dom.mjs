// Dependency-free DOM surface shared by optimized-host tests.

export class FakeNode {
  constructor() {
    this.childNodes = [];
    this.parentNode = null;
  }
  appendChild(child) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.childNodes.push(child);
    return child;
  }
  insertBefore(child, before) {
    if (child === before) return child;
    if (child.parentNode) child.parentNode.removeChild(child);
    const index = this.childNodes.indexOf(before);
    if (index < 0) throw new Error("insertBefore: reference node is not a child");
    child.parentNode = this;
    this.childNodes.splice(index, 0, child);
    return child;
  }
  removeChild(child) {
    const index = this.childNodes.indexOf(child);
    if (index >= 0) this.childNodes.splice(index, 1);
    child.parentNode = null;
    return child;
  }
  replaceChildren(...children) {
    for (const child of [...this.childNodes]) this.removeChild(child);
    for (const child of children) this.appendChild(child);
  }
}

export class FakeText extends FakeNode {
  constructor(value) {
    super();
    this.value = String(value);
  }
  get textContent() {
    return this.value;
  }
  set textContent(value) {
    this.value = String(value);
  }
}

class FakeStyle {
  constructor(owner) {
    this.owner = owner;
    this.values = new Map();
  }
  load(value) {
    this.values.clear();
    for (const declaration of String(value).split(";")) {
      if (declaration === "") continue;
      const separator = declaration.indexOf(":");
      if (separator > 0) this.values.set(declaration.slice(0, separator), declaration.slice(separator + 1));
    }
  }
  setProperty(name, value) {
    this.values.set(String(name), String(value));
    this.owner.attributes.set(
      "style",
      [...this.values].map(([property, current]) => `${property}:${current}`).join(";"),
    );
  }
  getPropertyValue(name) {
    return this.values.get(String(name)) || "";
  }
  clear() {
    this.values.clear();
  }
}

export class FakeElement extends FakeNode {
  constructor(tag) {
    super();
    this.tag = tag;
    this.attributes = new Map();
    this.listeners = new Map();
    this.style = new FakeStyle(this);
  }
  setAttribute(name, value) {
    this.attributes.set(name, String(value));
    if (name === "style") this.style.load(value);
  }
  removeAttribute(name) {
    this.attributes.delete(name);
    if (name === "style") this.style.clear();
  }
  addEventListener(name, listener) {
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name).add(listener);
  }
  removeEventListener(name, listener) {
    this.listeners.get(name)?.delete(listener);
  }
  dispatchEvent(event) {
    for (const listener of [...(this.listeners.get(event.type) || [])]) listener(event);
  }
  get textContent() {
    return this.childNodes.map((child) => child.textContent).join("");
  }
  set textContent(value) {
    this.childNodes = [new FakeText(value)];
  }
}

export const fakeDocument = Object.freeze({
  createElement: (tag) => new FakeElement(tag),
  createTextNode: (value) => new FakeText(value),
});
