declare module 'china-map-geojson' {
  import type { FeatureCollection } from 'geojson';

  const value: {
    ChinaData: FeatureCollection;
    ProvinceData: Record<string, FeatureCollection>;
  };

  export default value;
}

declare module 'china-map-geojson/lib/china' {
  import type { FeatureCollection } from 'geojson';

  const value: FeatureCollection;

  export default value;
}
