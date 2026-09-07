# \LibraryAnalyticsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_library_analytics_component_actions**](LibraryAnalyticsApi.md#get_library_analytics_component_actions) | **GET** /v1/analytics/libraries/{file_key}/component/actions | Get library analytics component action data.
[**get_library_analytics_component_usages**](LibraryAnalyticsApi.md#get_library_analytics_component_usages) | **GET** /v1/analytics/libraries/{file_key}/component/usages | Get library analytics component usage data.
[**get_library_analytics_style_actions**](LibraryAnalyticsApi.md#get_library_analytics_style_actions) | **GET** /v1/analytics/libraries/{file_key}/style/actions | Get library analytics style action data.
[**get_library_analytics_style_usages**](LibraryAnalyticsApi.md#get_library_analytics_style_usages) | **GET** /v1/analytics/libraries/{file_key}/style/usages | Get library analytics style usage data.
[**get_library_analytics_variable_actions**](LibraryAnalyticsApi.md#get_library_analytics_variable_actions) | **GET** /v1/analytics/libraries/{file_key}/variable/actions | Get library analytics variable action data.
[**get_library_analytics_variable_usages**](LibraryAnalyticsApi.md#get_library_analytics_variable_usages) | **GET** /v1/analytics/libraries/{file_key}/variable/usages | Get library analytics variable usage data.



## get_library_analytics_component_actions

> models::InlineObject39 get_library_analytics_component_actions(file_key, group_by, cursor, start_date, end_date)
Get library analytics component action data.

Returns a list of library analytics component actions data broken down by the requested dimension.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File key of the library to fetch analytics data for. | [required] |
**group_by** | **String** | A dimension to group returned analytics data by. | [required] |
**cursor** | Option<**String**> | Cursor indicating what page of data to fetch. Obtained from prior API call. |  |
**start_date** | Option<**String**> | ISO 8601 date string (YYYY-MM-DD) of the earliest week to include. Dates are rounded back to the nearest start of a week. Defaults to one year prior. |  |
**end_date** | Option<**String**> | ISO 8601 date string (YYYY-MM-DD) of the latest week to include. Dates are rounded forward to the nearest end of a week. Defaults to the latest computed week. |  |

### Return type

[**models::InlineObject39**](inline_object_39.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_library_analytics_component_usages

> models::InlineObject40 get_library_analytics_component_usages(file_key, group_by, cursor)
Get library analytics component usage data.

Returns a list of library analytics component usage data broken down by the requested dimension.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File key of the library to fetch analytics data for. | [required] |
**group_by** | **String** | A dimension to group returned analytics data by. | [required] |
**cursor** | Option<**String**> | Cursor indicating what page of data to fetch. Obtained from prior API call. |  |

### Return type

[**models::InlineObject40**](inline_object_40.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_library_analytics_style_actions

> models::InlineObject41 get_library_analytics_style_actions(file_key, group_by, cursor, start_date, end_date)
Get library analytics style action data.

Returns a list of library analytics style actions data broken down by the requested dimension.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File key of the library to fetch analytics data for. | [required] |
**group_by** | **String** | A dimension to group returned analytics data by. | [required] |
**cursor** | Option<**String**> | Cursor indicating what page of data to fetch. Obtained from prior API call. |  |
**start_date** | Option<**String**> | ISO 8601 date string (YYYY-MM-DD) of the earliest week to include. Dates are rounded back to the nearest start of a week. Defaults to one year prior. |  |
**end_date** | Option<**String**> | ISO 8601 date string (YYYY-MM-DD) of the latest week to include. Dates are rounded forward to the nearest end of a week. Defaults to the latest computed week. |  |

### Return type

[**models::InlineObject41**](inline_object_41.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_library_analytics_style_usages

> models::InlineObject42 get_library_analytics_style_usages(file_key, group_by, cursor)
Get library analytics style usage data.

Returns a list of library analytics style usage data broken down by the requested dimension.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File key of the library to fetch analytics data for. | [required] |
**group_by** | **String** | A dimension to group returned analytics data by. | [required] |
**cursor** | Option<**String**> | Cursor indicating what page of data to fetch. Obtained from prior API call. |  |

### Return type

[**models::InlineObject42**](inline_object_42.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_library_analytics_variable_actions

> models::InlineObject43 get_library_analytics_variable_actions(file_key, group_by, cursor, start_date, end_date)
Get library analytics variable action data.

Returns a list of library analytics variable actions data broken down by the requested dimension.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File key of the library to fetch analytics data for. | [required] |
**group_by** | **String** | A dimension to group returned analytics data by. | [required] |
**cursor** | Option<**String**> | Cursor indicating what page of data to fetch. Obtained from prior API call. |  |
**start_date** | Option<**String**> | ISO 8601 date string (YYYY-MM-DD) of the earliest week to include. Dates are rounded back to the nearest start of a week. Defaults to one year prior. |  |
**end_date** | Option<**String**> | ISO 8601 date string (YYYY-MM-DD) of the latest week to include. Dates are rounded forward to the nearest end of a week. Defaults to the latest computed week. |  |

### Return type

[**models::InlineObject43**](inline_object_43.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_library_analytics_variable_usages

> models::InlineObject44 get_library_analytics_variable_usages(file_key, group_by, cursor)
Get library analytics variable usage data.

Returns a list of library analytics variable usage data broken down by the requested dimension.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File key of the library to fetch analytics data for. | [required] |
**group_by** | **String** | A dimension to group returned analytics data by. | [required] |
**cursor** | Option<**String**> | Cursor indicating what page of data to fetch. Obtained from prior API call. |  |

### Return type

[**models::InlineObject44**](inline_object_44.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

