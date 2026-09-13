// The runtime exposes modular entry points; DefinitelyTyped covers the root API.
declare module "css-tree/parser" {
  import { parse } from "css-tree";
  export default parse;
}
declare module "css-tree/generator" {
  import { generate } from "css-tree";
  export default generate;
}
declare module "css-tree/walker" {
  import { walk } from "css-tree";
  export default walk;
}
declare module "css-tree/utils" {
  export { ident } from "css-tree";
}
