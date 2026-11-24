/** Vite turns an imported image into its served URL. */
declare module "*.png" {
  const url: string;
  export default url;
}
