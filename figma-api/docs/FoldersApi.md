# \FoldersApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_folder_files**](FoldersApi.md#get_folder_files) | **GET** /v2/folders/{folder_id}/files | Get files in a folder
[**get_folder_folders**](FoldersApi.md#get_folder_folders) | **GET** /v2/folders/{folder_id}/folders | Get subfolders in a folder
[**get_folder_meta**](FoldersApi.md#get_folder_meta) | **GET** /v2/folders/{folder_id}/meta | Get folder metadata
[**get_team_folders**](FoldersApi.md#get_team_folders) | **GET** /v2/teams/{team_id}/folders | Get top-level folders in a team



## get_folder_files

> models::InlineObject11 get_folder_files(folder_id, branch_data)
Get files in a folder

Get a list of the files directly within the specified folder.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**folder_id** | **String** | ID of the folder to list files from | [required] |
**branch_data** | Option<**bool**> | Returns branch metadata in the response for each main file with a branch inside the folder. |  |[default to false]

### Return type

[**models::InlineObject11**](inline_object_11.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_folder_folders

> models::InlineObject9 get_folder_folders(folder_id)
Get subfolders in a folder

Get a list of the direct subfolders within the specified folder.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**folder_id** | **String** | ID of the parent folder to list subfolders from | [required] |

### Return type

[**models::InlineObject9**](inline_object_9.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_folder_meta

> models::InlineObject10 get_folder_meta(folder_id)
Get folder metadata

Get metadata for a folder (name, thumbnail, file count, timestamps) without enumerating its files.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**folder_id** | **String** | ID of the folder to get metadata for | [required] |

### Return type

[**models::InlineObject10**](inline_object_10.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_team_folders

> models::InlineObject8 get_team_folders(team_id)
Get top-level folders in a team

Get a list of the top-level folders (previously called \"projects\") within the specified team. Subfolders can be traversed with the GET /v2/folders/{folder_id}/folders endpoint. It is not possible to programmatically obtain team IDs. To obtain a team ID, navigate to the team page in the Figma file browser. The team ID is present in the URL after the word team. For example, in `https://www.figma.com/files/181033233908053158/team/1535685101263221741`, the team ID is `1535685101263221741`.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**team_id** | **String** | ID of the team to list folders from | [required] |

### Return type

[**models::InlineObject8**](inline_object_8.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

